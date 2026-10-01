#!/usr/bin/env python3
"""Package a prebuilt desktop binary without third-party Python packages."""
import argparse, hashlib, pathlib, plistlib, re, shutil, subprocess, sys, tarfile, tempfile, zipfile

def main():
    parser=argparse.ArgumentParser()
    parser.add_argument('--target', required=True, choices=[f'{arch}-{platform}' for arch in ('aarch64', 'x86_64') for platform in ('apple-darwin', 'pc-windows-msvc', 'unknown-linux-gnu')])
    parser.add_argument('--version', required=True)
    parser.add_argument('--output', type=pathlib.Path, required=True)
    parser.add_argument('--binary', type=pathlib.Path)
    args=parser.parse_args()
    if not re.fullmatch(r'[0-9]{4}\.[0-9]{1,2}\.[0-9]{1,2}\.[0-9]+', args.version):
        parser.error('--version must be YYYY.MM.DD.RELEASE')
    architecture='aarch64' if args.target.startswith('aarch64') else 'x86_64'
    platform='macos' if 'apple' in args.target else 'windows' if 'windows' in args.target else 'linux'
    binary=args.binary or pathlib.Path('target') / args.target / 'release' / ('xfer-desktop.exe' if platform=='windows' else 'xfer-desktop')
    args.output.mkdir(parents=True,exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='xfer-package-') as temporary:
        root=pathlib.Path(temporary)
        if platform=='macos':
            package=root/'XFER.app'; contents=package/'Contents'; (contents/'MacOS').mkdir(parents=True)
            shutil.copy2(binary,contents/'MacOS'/'xfer-desktop')
            (contents/'Resources').mkdir()
            shutil.copy2('desktop/assets/xfer.icns', contents/'Resources'/'xfer.icns')
            (contents/'MacOS'/'xfer-desktop').chmod(0o755)
            (contents/'Info.plist').write_bytes(plistlib.dumps({'CFBundleIconFile':'xfer.icns','CFBundleName':'XFER','CFBundleDisplayName':'XFER','CFBundleIdentifier':'com.cdenihan.xfer','CFBundleExecutable':'xfer-desktop','CFBundlePackageType':'APPL','CFBundleShortVersionString':'.'.join(str(int(part)) for part in args.version.split('.')[:3]),'CFBundleVersion':str(int(args.version.split('.')[-1])),'XFERReleaseVersion':args.version,'NSHighResolutionCapable':True,'NSLocalNetworkUsageDescription':'XFER discovers receivers and transfers files directly on your local network.'}))
        else:
            package=root/'xfer-desktop'; package.mkdir()
            executable='xfer-desktop.exe' if platform=='windows' else 'xfer-desktop'
            shutil.copy2(binary,package/executable)
            shutil.copy2('desktop/assets/xfer.png', package/'xfer.png')
            shutil.copy2('desktop/assets/xfer.ico' if platform=='windows' else 'desktop/assets/xfer.svg', package/('xfer.ico' if platform=='windows' else 'xfer.svg'))
            if platform=='linux':
                (package/executable).chmod(0o755)
                (package/'xfer.desktop').write_text('[Desktop Entry]\nType=Application\nName=XFER\nComment=Private local file transfer and sync\nExec=xfer-desktop\nIcon=xfer\nStartupWMClass=com.cdenihan.xfer\nTerminal=false\nCategories=Network;Utility;\n')
                (package/'install-desktop.sh').write_text('''#!/bin/sh
set -eu
source_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
install_dir="$HOME/.local/bin"
launcher_dir="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
icon_dir="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/256x256/apps"
mkdir -p "$install_dir" "$launcher_dir" "$icon_dir"
cp "$source_dir/xfer.png" "$icon_dir/xfer.png"
cp "$source_dir/xfer-desktop" "$install_dir/xfer-desktop"
chmod 755 "$install_dir/xfer-desktop"
python3 - "$install_dir/xfer-desktop" "$source_dir/xfer.desktop" "$launcher_dir/com.cdenihan.xfer.desktop" <<'INSTALL_PY'
import pathlib,sys
# Desktop Exec has its own quoting rules, including literal percent escapes.
path=sys.argv[1].replace('\\\\','\\\\\\\\').replace('"','\\\\"').replace('`','\\\\`').replace('$','\\\\$').replace('%','%%')
text=pathlib.Path(sys.argv[2]).read_text().replace('Exec=xfer-desktop','Exec="'+path+'"')
pathlib.Path(sys.argv[3]).write_text(text)
INSTALL_PY
''')
                (package/'install-desktop.sh').chmod(0o755)
        notices=package/'Contents'/'Resources' if platform=='macos' else package
        shutil.copy2('THIRD_PARTY_NOTICES.md',notices/'THIRD_PARTY_NOTICES.md')
        shutil.copy2('desktop/LICENSE-GPUI-APACHE',notices/'LICENSE-GPUI-APACHE')
        if platform=='macos' and sys.platform=='darwin':
            with binary.open('rb') as executable_file:
                magic=executable_file.read(4)
            if magic in (b'\xcf\xfa\xed\xfe', b'\xfe\xed\xfa\xcf', b'\xca\xfe\xba\xbe'):
                # Apple's linker signs ARM64 binaries ad hoc. Seal the assembled
                # bundle too, otherwise adding icon resources invalidates that seal.
                # This does not provide Developer ID signing or notarization.
                subprocess.run(['codesign', '--force', '--sign', '-', '--timestamp=none', str(package)], check=True, capture_output=True)
                subprocess.run(['codesign', '--verify', '--deep', '--strict', str(package)], check=True, capture_output=True)

        archive=args.output/f'xfer-desktop-{platform}-{architecture}.{"zip" if platform=="windows" else "tar.gz"}'
        if platform=='windows':
            with zipfile.ZipFile(archive,'w',zipfile.ZIP_DEFLATED) as output:
                for file in package.rglob('*'): output.write(file,file.relative_to(root))
        else:
            with tarfile.open(archive,'w:gz') as output: output.add(package,arcname=package.name)
        hasher=hashlib.sha256()
        with archive.open('rb') as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b''):
                hasher.update(chunk)
        digest=hasher.hexdigest()
        archive.with_name(archive.name+'.sha256').write_text(f'{digest}  {archive.name}\n')
        print(archive)

if __name__=='__main__':main()
