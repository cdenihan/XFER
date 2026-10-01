#!/usr/bin/env python3
"""Verify archive contents and Linux launcher installation with disposable binaries."""
import hashlib
import os
from pathlib import Path
import plistlib
import subprocess
import tarfile
import tempfile
import unittest
import zipfile

ROOT = Path(__file__).resolve().parent.parent


class PackagingTests(unittest.TestCase):
    def package(self, root, target, name):
        binary = root / name
        binary.write_bytes(b'fixture binary\n')
        subprocess.run(['python3', str(ROOT / 'scripts/package-desktop.py'),
                        '--target', target, '--version', '2026.10.01.1',
                        '--binary', str(binary), '--output', str(root / 'dist')],
                       cwd=ROOT, check=True, capture_output=True)
        archive = next(path for path in (root / 'dist').iterdir() if not path.name.endswith('.sha256'))
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        self.assertEqual(archive.with_name(archive.name + '.sha256').read_text(),
                         f'{digest}  {archive.name}\n')
        return archive

    def test_macos_bundle(self):
        with tempfile.TemporaryDirectory() as temporary:
            archive = self.package(Path(temporary), 'aarch64-apple-darwin', 'xfer-desktop')
            with tarfile.open(archive) as bundle:
                info = plistlib.loads(bundle.extractfile('XFER.app/Contents/Info.plist').read())
                self.assertEqual(info['CFBundleExecutable'], 'xfer-desktop')
                self.assertEqual(info['CFBundleShortVersionString'], '2026.10.1')
                self.assertEqual(info['CFBundleVersion'], '1')
                self.assertEqual(info['XFERReleaseVersion'], '2026.10.01.1')
                self.assertTrue(info['NSLocalNetworkUsageDescription'])
                self.assertEqual(info['CFBundleIconFile'], 'xfer.icns')
                self.assertEqual(bundle.extractfile('XFER.app/Contents/Resources/xfer.icns').read()[:4], b'icns')
                self.assertEqual(bundle.getmember('XFER.app/Contents/MacOS/xfer-desktop').mode & 0o777, 0o755)
                self.assertIsNotNone(bundle.getmember('XFER.app/Contents/Resources/LICENSE-GPUI-APACHE'))

    def test_windows_portable(self):
        with tempfile.TemporaryDirectory() as temporary:
            archive = self.package(Path(temporary), 'x86_64-pc-windows-msvc', 'xfer-desktop.exe')
            with zipfile.ZipFile(archive) as bundle:
                self.assertEqual(bundle.read('xfer-desktop/xfer-desktop.exe'), b'fixture binary\n')
                self.assertIn('xfer-desktop/THIRD_PARTY_NOTICES.md', bundle.namelist())
                self.assertEqual(bundle.read('xfer-desktop/xfer.ico')[:4], b'\0\0\1\0')

    @unittest.skipIf(os.name == 'nt', 'POSIX launcher')
    def test_linux_launcher_escapes_paths(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            archive = self.package(root, 'x86_64-unknown-linux-gnu', 'xfer-desktop')
            with tarfile.open(archive) as bundle:
                for name in ('xfer-desktop', 'xfer.desktop', 'install-desktop.sh', 'xfer.png'):
                    destination = root / 'extracted/xfer-desktop' / name
                    destination.parent.mkdir(parents=True, exist_ok=True)
                    destination.write_bytes(bundle.extractfile('xfer-desktop/' + name).read())
            home = root / 'home with $value and 50%'
            env = dict(os.environ, HOME=str(home), XDG_DATA_HOME=str(home / 'share'))
            subprocess.run(['sh', str(root / 'extracted/xfer-desktop/install-desktop.sh')],
                           env=env, check=True, capture_output=True)
            self.assertEqual((home / '.local/bin/xfer-desktop').read_bytes(), b'fixture binary\n')
            launcher = (home / 'share/applications/com.cdenihan.xfer.desktop').read_text()
            self.assertIn('Exec="', launcher)
            self.assertIn(r'\$value', launcher)
            self.assertIn('50%%', launcher)
            self.assertIn('Icon=xfer', launcher)
            self.assertTrue((home / 'share/icons/hicolor/256x256/apps/xfer.png').is_file())


if __name__ == '__main__':
    unittest.main()
