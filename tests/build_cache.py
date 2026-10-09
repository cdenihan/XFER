#!/usr/bin/env python3
"""Verify frontend build caching and added/modified/deleted public assets."""
import os, subprocess
from pathlib import Path
root=Path(__file__).resolve().parent.parent
probe=root/'web/public/cache-probe.txt'
assert not probe.exists()
env=dict(os.environ)
def run():
 result=subprocess.run(['zig','build','web','--summary','all'],cwd=root,env=env,capture_output=True,text=True,check=True)
 return result.stdout+result.stderr
try:
 run()
 assert 'run bun (ui) cached' in run(), 'Unchanged frontend must be cached'
 probe.write_text('first cache invalidation probe')
 assert 'run bun (ui) success' in run(), 'Added public asset must invalidate the cache'
 assert 'run bun (ui) cached' in run()
 probe.write_text('modified cache invalidation probe')
 assert 'run bun (ui) success' in run(), 'Changed asset content must invalidate the cache'
 probe.unlink()
 assert 'run bun (ui) cached' in run(), 'Removing the probe must restore the original cached pack'
 assert 'run bun (ui) cached' in run()
 print('PASS cached build and added/modified/deleted asset invalidation')
finally:
 probe.unlink(missing_ok=True)
