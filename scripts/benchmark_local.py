#!/usr/bin/env python3
"""Alternating loopback benchmarks; stdlib only, independent SHA-256 checks."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import queue
import platform
import statistics
import subprocess
import tempfile
import threading
import time


def run(binary, source, destination):
    import socket
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    env = {**os.environ, 'XFER_TOKEN': 'local-benchmark-only-' + 'a' * 32}
    receiver = subprocess.Popen([str(binary), '--json', 'receive', '--yes', '--once', '--no-discovery', '--bind', '127.0.0.1', '--port', str(port), '--output', str(destination)], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    ready = queue.Queue()
    def consume():
        for line in receiver.stdout:
            if json.loads(line)['event'] == 'listening':
                ready.put(True)
        ready.put(False)
    thread = threading.Thread(target=consume, daemon=True)
    thread.start()
    try:
        assert ready.get(timeout=15)
        start = time.perf_counter()
        sender = subprocess.run([str(binary), '--json', 'send', str(source), '--to', '127.0.0.1', '--port', str(port), '--yes'], env=env, capture_output=True, text=True, timeout=120)
        elapsed = time.perf_counter() - start
        assert sender.returncode == 0, sender.stderr
        assert receiver.wait(timeout=15) == 0, receiver.stderr.read()
    finally:
        if receiver.poll() is None:
            receiver.kill()
            receiver.wait()
        thread.join(timeout=5)
        receiver.stdout.close()
        receiver.stderr.close()
    def inventory(root):
        files = [root] if root.is_file() else sorted(root.rglob('*'))
        result = {}
        for file in files:
            if file.is_file():
                with file.open('rb') as stream:
                    digest = hashlib.sha256()
                    for block in iter(lambda: stream.read(1024 * 1024), b''):
                        digest.update(block)
                    result[str(file.relative_to(root.parent))] = digest.hexdigest()
        return result
    assert inventory(source) == inventory(destination / source.name)
    return elapsed


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('before', type=Path)
    parser.add_argument('after', type=Path)
    parser.add_argument('--trials', type=int, default=5)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    if args.trials < 1:
        parser.error('--trials must be positive')
    results = {'platform': platform.platform(), 'machine': platform.machine(), 'binary_sha256': {label: hashlib.sha256(getattr(args, label).read_bytes()).hexdigest() for label in ('before', 'after')}, 'scope': 'loopback, warm filesystem cache; includes planning, encryption, sync and acknowledgement', 'trials': []}
    with tempfile.TemporaryDirectory(prefix='xfer-local-benchmark-') as tmp:
        root = Path(tmp)
        large = root / 'large.bin'
        with large.open('wb') as stream:
            for _ in range(128):
                stream.write(os.urandom(1024 * 1024))
        small = root / 'small'
        small.mkdir()
        for i in range(1000):
            (small / str(i)).write_bytes(os.urandom(4096))
        for dataset, source in [('large', large), ('small', small)]:
            for trial_index in range(args.trials + 1):
                order = ['before', 'after'] if trial_index % 2 == 0 else ['after', 'before']
                for label in order:
                    elapsed = run(getattr(args, label).resolve(), source, root / f'{dataset}-{label}-{trial_index}')
                    result = {'dataset': dataset, 'binary': label, 'warmup': trial_index == 0, 'wall_s': elapsed}
                    results['trials'].append(result)
                    print(json.dumps(result), flush=True)
    results['medians_s'] = {dataset: {label: statistics.median(t['wall_s'] for t in results['trials'] if t['dataset'] == dataset and t['binary'] == label and not t['warmup']) for label in ['before', 'after']} for dataset in ['large', 'small']}
    args.output.write_text(json.dumps(results, indent=2) + '\n')
    print(json.dumps(results['medians_s'], indent=2))


if __name__ == '__main__':
    main()
