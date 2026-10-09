#!/usr/bin/env python3
"""Build single-executable archives with the Bun/Vite+ UI compiled into Zig."""
import hashlib
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parent.parent
TARGETS = ['x86_64-linux-musl', 'aarch64-linux-musl', 'x86_64-macos', 'aarch64-macos', 'x86_64-windows', 'aarch64-windows']


def main():
    version = (ROOT / 'VERSION').read_text().strip()
    zon = (ROOT / 'build.zig.zon').read_text()
    assert re.search(r'\.version\s*=\s*"' + re.escape(version) + '"', zon), 'VERSION and build.zig.zon must match'
    output = ROOT / 'dist' / version
    output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='xfer-release-') as tmp:
        for target in TARGETS:
            prefix = Path(tmp) / target
            subprocess.run(['zig', 'build', '-Dtarget=' + target, '-Doptimize=ReleaseSafe', '--prefix', str(prefix)], cwd=ROOT, check=True)
            windows = 'windows' in target
            binary = prefix / 'bin' / ('xfer.exe' if windows else 'xfer')
            name = f'xfer-{version}-{target}'
            extension = '.zip' if windows else '.tar.gz'
            archive = output / (name + extension)
            if windows:
                with zipfile.ZipFile(archive, 'w', zipfile.ZIP_DEFLATED) as z:
                    z.write(binary, binary.name)
                    for file in ('README.md', 'SECURITY.md', 'VERSION'):
                        z.write(ROOT / file, file)
            else:
                with tarfile.open(archive, 'w:gz') as t:
                    t.add(binary, arcname=binary.name)
                    for file in ('README.md', 'SECURITY.md', 'VERSION'):
                        t.add(ROOT / file, arcname=file)
            checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
            (output / (archive.name + '.sha256')).write_text(f'{checksum}  {archive.name}\n')
            print(archive.name)
    shutil.copyfile(ROOT / 'VERSION', output / 'VERSION')


if __name__ == '__main__':
    main()
