#!/usr/bin/env python3
"""Locate an installed Tailcat or build a checksum-pinned upstream release locally."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import urllib.request

VERSION = '0.7.0'
URL = f'https://github.com/tailscale/tailcat/archive/refs/tags/v{VERSION}.tar.gz'
SHA256 = '54a97d9046d0bf2afbf99987ff630fc425ee79272c6c7ccd645a49a076d3cecb'
ROOT = Path(__file__).resolve().parent.parent

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', action='store_true', help='Build pinned source instead of using an existing helper')
    parser.add_argument('--output', type=Path, default=ROOT / '.tools' / ('tailcat.exe' if os.name == 'nt' else 'tailcat'))
    args = parser.parse_args()
    if not args.source:
        existing = shutil.which('tailcat')
        if not existing:
            parser.error('Tailcat is absent. Use brew install tailcat on macOS, or --source with Go 1.27.1+.')
        version = subprocess.check_output([existing, 'version'], text=True).strip()
        print(json.dumps({'path': existing, 'version': version, 'method': 'installed'}))
        return
    go = shutil.which('go')
    if not go: parser.error('Source builds require an installed Go 1.27.1+ toolchain.')
    output = args.output.resolve()
    if output.exists(): parser.error(f'Output already exists: {output}; choose another --output.')
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='xfer-tailcat-source-') as temporary:
        temporary = Path(temporary).resolve()
        archive = temporary / 'source.tar.gz'
        with urllib.request.urlopen(URL, timeout=60) as response:
            archive.write_bytes(response.read())
        if hashlib.sha256(archive.read_bytes()).hexdigest() != SHA256:
            raise RuntimeError('Upstream source checksum does not match the pinned release')
        with tarfile.open(archive) as source:
            regular, links = [], []
            for item in source.getmembers():
                target = (temporary / item.name).resolve()
                if temporary not in target.parents:
                    raise RuntimeError('Unsafe path in source archive')
                if item.isdir() or item.isfile(): regular.append(item)
                elif item.issym() or item.islnk(): links.append(item)
                else: raise RuntimeError('Unsupported entry in source archive')
            source.extractall(temporary, members=regular)
            # Upstream embeds its README through an internal symlink. Materialize
            # internal file links, avoiding symlink privileges and link traversal.
            for item in links:
                target = temporary / item.name
                origin = (target.parent / item.linkname if item.issym() else temporary / item.linkname).resolve()
                if temporary not in origin.parents or not origin.is_file():
                    raise RuntimeError('Unsafe link in source archive')
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(origin, target)
        folder = temporary / f'tailcat-{VERSION}'
        built = temporary / output.name
        env = {**os.environ, 'CGO_ENABLED': '0', 'GOTOOLCHAIN': 'local'}
        # XFER only uses port forwarding, so SSH/browser modules are unnecessary.
        subprocess.run([go, 'build', '-trimpath', '-tags=ts_omit_ssh', '-ldflags=-s -w -X main.version=v' + VERSION, '-o', str(built), './cmd/tailcat'], cwd=folder, env=env, check=True)
        subprocess.run([str(built), 'version'], check=True)
        shutil.copy2(built, output)
        shutil.copyfile(folder / 'LICENSE', output.with_name(output.name + '.LICENSE'))
    record = {'path': str(output), 'version': VERSION, 'method': 'source', 'source_url': URL, 'source_sha256': SHA256, 'binary_sha256': hashlib.sha256(output.read_bytes()).hexdigest()}
    output.with_name(output.name + '.provenance.json').write_text(json.dumps(record, indent=2) + '\n')
    print(json.dumps(record))
if __name__ == '__main__': main()
