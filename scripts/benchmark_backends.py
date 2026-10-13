#!/usr/bin/env python3
"""Paired, verified XFER desktop transfers: native TCP versus installed Tailcat."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import statistics
import subprocess
import shutil
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / 'tests'))

def inventory(path):
    if path.is_file(): return {'file': hashlib.sha256(path.read_bytes()).hexdigest()}
    result = {}
    for item in sorted(path.rglob('*')):
        key = str(item.relative_to(path))
        result[key] = hashlib.sha256(item.read_bytes()).hexdigest() if item.is_file() else 'directory'
    return result

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary', type=Path)
    parser.add_argument('--trials', type=int, default=5)
    parser.add_argument('--large-mib', type=int, default=32)
    parser.add_argument('--small-files', type=int, default=500)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    if not 1 <= args.trials <= 50 or not 1 <= args.large_mib <= 1024 or not 1 <= args.small_files <= 10000:
        parser.error('Dataset/trial limits exceeded')
    # desktop's existing test adapter takes its executable from argv[1].
    sys.argv = [sys.argv[0], str(args.binary.resolve())]
    import desktop
    os.environ['TS_DEBUG_TAILCAT_LOCAL_DERP'] = '1'
    os.environ['TAILCAT_DERPMAP_URL'] = 'none'
    report = {'measured_at': datetime.now(timezone.utc).isoformat(), 'platform': platform.platform(), 'binary_sha256': hashlib.sha256(args.binary.read_bytes()).hexdigest(), 'scope': 'Same-host loopback; isolated local DERP bootstrap. Actual tunnel path unclassified. Not a WAN benchmark.', 'approval': 'Both real APIs compare matching codes before approval; polling latency included.', 'tailcat_version': subprocess.check_output([os.environ.get('XFER_TAILCAT_BIN') or shutil.which('tailcat'), 'version'], text=True).strip(), 'trials': []}
    with tempfile.TemporaryDirectory(prefix='xfer-backends-') as directory:
        root = Path(directory)
        large = root / 'large.bin'
        with large.open('wb') as stream:
            for _ in range(args.large_mib): stream.write(os.urandom(1024 * 1024))
        small = root / 'small'; small.mkdir()
        (small / 'empty').mkdir()
        for i in range(args.small_files): (small / f'{i:05}.bin').write_bytes(os.urandom(4096))
        receiver = desktop.Desktop(root / 'receiver', 'Benchmark receiver')
        sender = desktop.Desktop(root / 'sender', 'Benchmark sender')
        try:
            assert receiver.state()['tailcat']['available'] and sender.state()['tailcat']['available']
            start = time.perf_counter()
            receiver.request('/api/tailcat/start', {})
            invite = receiver.wait(lambda s: s['tailcat']['enabled'])['tailcat']['invite']
            report['tailcat_listener_startup_s'] = time.perf_counter() - start
            for dataset, source in [('large', large), ('small', small)]:
                expected = inventory(source)
                bytes_total = source.stat().st_size if source.is_file() else sum(p.stat().st_size for p in source.rglob('*') if p.is_file())
                for trial in range(args.trials + 1):
                    for mode in (['nearby', 'tailcat'] if trial % 2 == 0 else ['tailcat', 'nearby']):
                        start = time.perf_counter()
                        if source.is_file():
                            sender.prepare(source.name, [(source.name, source.read_bytes())])
                        else:
                            files = [(source.name + '/' + str(p.relative_to(source)), p.read_bytes()) for p in sorted(source.rglob('*')) if p.is_file()]
                            folders = [source.name] + [source.name + '/' + str(p.relative_to(source)) for p in sorted(source.rglob('*')) if p.is_dir()]
                            sender.prepare(source.name, files, folders)
                        staged = time.perf_counter()
                        sender.request('/api/send', {'to': invite if mode == 'tailcat' else f'127.0.0.1:{receiver.port}'})
                        left = sender.wait(lambda s: s['pending'])['pending']
                        right = receiver.wait(lambda s: s['pending'])['pending']
                        assert left['code'] == right['code']
                        ready = time.perf_counter()
                        sender.approve(left); receiver.approve(right)
                        delivered = receiver.wait(lambda s: s['phase'] == 'received' and not s['busy'])['message']
                        sender.wait(lambda s: s['phase'] == 'sent' and not s['busy'])
                        end = time.perf_counter()
                        output = receiver.output / delivered
                        assert inventory(output) == expected, 'Independent delivery hashes differ'
                        if output.is_file(): output.unlink()
                        else:
                            shutil.rmtree(output)
                        result = {'dataset': dataset, 'mode': mode, 'warmup': trial == 0, 'bytes': bytes_total, 'staging_s': staged-start, 'connect_and_consent_s': ready-staged, 'transfer_s': end-ready, 'end_to_end_s': end-start, 'verified': True}
                        report['trials'].append(result)
                        print(json.dumps(result), flush=True)
        finally:
            sender.close(); receiver.close()
    report['medians'] = {dataset: {mode: {metric: statistics.median(row[metric] for row in report['trials'] if row['dataset'] == dataset and row['mode'] == mode and not row['warmup']) for metric in ['staging_s','connect_and_consent_s','transfer_s','end_to_end_s']} for mode in ['nearby','tailcat']} for dataset in ['large','small']}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report['medians'], indent=2))
if __name__ == '__main__': main()
