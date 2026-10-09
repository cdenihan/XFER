#!/usr/bin/env python3
"""Run the lone release binary away from the checkout with no runtime on PATH."""
import gzip
import os
import re
from pathlib import Path
import shutil
import subprocess
import tempfile
import desktop


def run():
    original = desktop.BINARY
    with tempfile.TemporaryDirectory(prefix='xfer-standalone-') as directory:
        root = Path(directory)
        install = root / 'install'
        install.mkdir()
        binary = install / Path(original).name
        shutil.copy2(original, binary)
        desktop.BINARY = str(binary)
        saved_path = os.environ.get('PATH')
        os.environ['PATH'] = str(install)
        try:
            version = subprocess.run([str(binary), '--version'], cwd=install, capture_output=True, check=True)
            assert b'Zig 0.17.0' in version.stdout
            app = desktop.Desktop(root / 'session', 'Standalone')
            page, _ = app.request('/', method='GET')
            assert b'/assets/' in page
            for asset in re.findall(rb'(?:src|href)="(/assets/[^\"]+)"', page):
                body, headers = app.request(asset.decode(), method='GET', headers={'Accept-Encoding': 'gzip'})
                assert dict(headers)['Content-Encoding'] == 'gzip'
                assert gzip.decompress(body), 'Standalone UI asset is missing'

            app.prepare('selected.bin', [('selected.bin', b'private selection')])
            app.close()
            assert set(install.iterdir()) == {binary}, 'An asset or native helper was extracted'
            assert set(app.root.iterdir()) == {app.output}, 'Unexpected runtime files were created'
        finally:
            desktop.BINARY = original
            if saved_path is None:
                del os.environ['PATH']
            else:
                os.environ['PATH'] = saved_path
    print('PASS one native binary, no runtime on PATH, embedded UI and no extracted assets/helpers')


if __name__ == '__main__':
    run()
