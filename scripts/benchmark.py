#!/usr/bin/env python3
"""Benchmark verified encrypted copies over SSH-controlled, direct LAN sockets.
Requires prebuilt binaries. Never installs software on the remote host.
"""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import platform
import queue
import resource
import secrets
import shlex
import shutil
import socket
import statistics
import subprocess
import tempfile
import threading
import time


def inventory(root):
    result = {}
    for path in sorted(root.rglob('*')):
        if path.is_file():
            digest = hashlib.sha256()
            with path.open('rb') as stream:
                for chunk in iter(lambda: stream.read(1024 * 1024), b''):
                    digest.update(chunk)
            result[path.relative_to(root).as_posix()] = digest.hexdigest()
    return result


def receiver(binary, implementation, output, token, config, port):
    command = [str(binary), '--json', 'receive', '--once', '--no-discovery', '--bind', '0.0.0.0', '--port', str(port), '--output', str(output)]
    if implementation == 'zig':
        command.append('--yes')
    process = subprocess.Popen(command, env={**os.environ, 'XFER_TOKEN': token, 'XFER_CONFIG_DIR': str(config)}, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    lines = []
    ready = queue.Queue()
    def consume():
        for line in process.stdout:
            lines.append(line)
            if 'listening on' in line.lower():
                ready.put(True)
        ready.put(False)
    reader = threading.Thread(target=consume, daemon=True)
    reader.start()
    if not ready.get(timeout=20):
        raise RuntimeError('Receiver not ready: ' + ''.join(lines) + process.stderr.read())
    return process, reader, lines


def worker(args):
    root = Path(args.root)
    assert root.name.startswith('xfer-benchmark-') and root.parent == Path('/tmp')
    binary = root / args.implementation
    token = (root / 'token').read_text().strip()
    path = root / args.dataset
    print(json.dumps({'ready': True, 'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
        'binary_bytes': binary.stat().st_size, 'version': subprocess.check_output([str(binary), '--version'], text=True).strip(),
        'platform': platform.platform(), 'machine': platform.machine(), 'cpu': next((line.split(':', 1)[1].strip() for line in (Path('/proc/cpuinfo').read_text().splitlines() if Path('/proc/cpuinfo').exists() else []) if line.startswith('Model')), platform.machine())}), flush=True)
    if input() != 'start':
        raise RuntimeError('Expected benchmark start signal')
    if args.implementation == 'rust':
        command = [str(binary), '--json', 'send', args.destination, str(path), '--port', str(args.port), '--accept-new']
    else:
        command = [str(binary), '--json', 'send', str(path), '--to', args.destination, '--port', str(args.port), '--yes']
    env = {**os.environ, 'XFER_TOKEN': token, 'XFER_CONFIG_DIR': str(root / 'rust-config')}
    before = resource.getrusage(resource.RUSAGE_CHILDREN)
    started = time.perf_counter()
    result = subprocess.run(command, env=env, capture_output=True, text=True, timeout=300)
    elapsed = time.perf_counter() - started
    if result.returncode:
        raise RuntimeError(result.stdout + result.stderr)
    after = resource.getrusage(resource.RUSAGE_CHILDREN)
    print(json.dumps({'wall_s': elapsed, 'sender_cpu_s': after.ru_utime + after.ru_stime - before.ru_utime - before.ru_stime, 'sender_peak_rss_kib': after.ru_maxrss / 1024 if platform.system() == 'Darwin' else after.ru_maxrss}), flush=True)


def run(args):
    remote, root = args.remote, args.root
    remote_root = Path(root)
    if remote_root.parent != Path('/tmp') or not remote_root.name.startswith('xfer-benchmark-'):
        raise ValueError('Use an isolated /tmp/xfer-benchmark-* directory on the remote host')
    metadata = {'measured_at': datetime.now(timezone.utc).isoformat(), 'receiver_platform': platform.platform(), 'receiver_machine': platform.machine(), 'binaries': {}, 'rust_commit': args.rust_commit, 'zig_commit': args.zig_commit, 'zig_build': args.zig_build}
    receiver_binaries = {implementation: {'version': subprocess.check_output([binary, '--version'], text=True).strip(),
        'binary_bytes': Path(binary).stat().st_size, 'binary_sha256': hashlib.sha256(Path(binary).read_bytes()).hexdigest()}
        for implementation, binary in (('rust', args.rust), ('zig', args.zig))}
    source = Path(__file__).resolve().parent.parent
    digest = hashlib.sha256()
    for path in sorted([*source.joinpath('src').rglob('*'), source / 'build.zig', source / 'build.zig.zon', source / 'VERSION']):
        if path.is_file():
            digest.update(path.relative_to(source).as_posix().encode() + b'\0' + path.read_bytes() + b'\0')
    metadata['zig_source_sha256'] = digest.hexdigest()
    ssh = ['ssh', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=10', remote]
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as probe:
        probe.connect((remote.split('@')[-1], 22))
        destination = probe.getsockname()[0]
    token = secrets.token_hex(32)
    with tempfile.TemporaryDirectory(prefix='xfer-benchmark-local-') as temporary:
        local = Path(temporary)
        (local / 'token').write_text(token)
        (local / 'token').chmod(0o600)
        large = local / 'large-file'
        large.mkdir()
        with (large / 'random.bin').open('wb') as f:
            for _ in range(args.large_mib):
                f.write(os.urandom(1024 * 1024))
        small = local / 'small-files'
        small.mkdir()
        for i in range(args.files):
            directory = small / f'group-{i % 10:02d}'
            directory.mkdir(exist_ok=True)
            (directory / f'file-{i:04d}.bin').write_bytes(os.urandom(4096))
        datasets = {'large-file': large, 'small-files': small}
        expected = {name: inventory(path) for name, path in datasets.items()}
        subprocess.run(['scp', '-r', str(local / 'token'), str(large), str(small), str(Path(__file__).resolve()), remote + ':' + root + '/'], check=True)
        results = []
        for dataset, path in datasets.items():
            payload = sum(p.stat().st_size for p in path.rglob('*') if p.is_file())
            for trial in range(args.trials + 1):
                order = ['rust', 'zig'] if trial % 2 == 0 else ['zig', 'rust']
                for implementation in order:
                    output = local / 'received'
                    output.mkdir()
                    with socket.socket() as socket_probe:
                        socket_probe.bind(('0.0.0.0', 0))
                        port = socket_probe.getsockname()[1]
                    binary = args.rust if implementation == 'rust' else args.zig
                    before = resource.getrusage(resource.RUSAGE_CHILDREN)
                    process, reader, lines = receiver(binary, implementation, output, token, local / 'rust-config', port)
                    child = None
                    try:
                        command = ['python3', root + '/benchmark.py', '--worker', '--root', root, '--implementation', implementation, '--dataset', dataset, '--destination', destination, '--port', str(port)]
                        # Generated root, IP address, and fixed arguments contain no shell metacharacters.
                        child = subprocess.Popen(ssh + [shlex.join(command)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                        ready_line = child.stdout.readline()
                        if not ready_line:
                            raise RuntimeError(child.stderr.read())
                        ready = json.loads(ready_line)
                        assert ready['ready']
                        metadata['sender_platform'] = ready['platform']
                        metadata['sender_cpu'] = ready['cpu']
                        metadata['sender_machine'] = ready['machine']
                        metadata['binaries'][implementation] = {'sender': {key: ready[key] for key in ('version', 'binary_bytes', 'binary_sha256')},
                            'receiver': receiver_binaries[implementation]}
                        child.stdin.write('start\n')
                        child.stdin.flush()
                        line = child.stdout.readline()
                        if not line:
                            raise RuntimeError(child.stderr.read())
                        finished = json.loads(line)
                        process.wait(timeout=300)
                        reader.join(timeout=5)
                        after = resource.getrusage(resource.RUSAGE_CHILDREN)
                        stderr = process.stderr.read()
                        assert process.returncode == 0, ''.join(lines) + stderr
                        assert inventory(output / dataset) == expected[dataset], 'Published bytes did not match source hashes'
                        remaining, stderr = child.communicate(timeout=30)
                        assert child.returncode == 0, (remaining, stderr)
                        elapsed = finished['wall_s']
                        row = {'dataset': dataset, 'implementation': implementation, 'trial': trial, 'warmup': trial == 0, 'bytes': payload, 'mib_s': payload / 1024**2 / elapsed, 'receiver_cpu_s': after.ru_utime + after.ru_stime - before.ru_utime - before.ru_stime, 'verified': True, **finished}
                        results.append(row)
                        print(json.dumps(row), flush=True)
                        Path(args.output).write_text(json.dumps({'remote': remote, 'direction': f'{metadata["sender_platform"]} ({metadata["sender_machine"]}) sender to {metadata["receiver_platform"]} ({metadata["receiver_machine"]}) receiver over direct LAN TCP', 'local_address': destination, **metadata, 'results': results}, indent=2) + '\n')
                    finally:
                        for running in (process, child):
                            if running is not None and running.poll() is None:
                                running.kill()
                                running.wait(timeout=10)
                        shutil.rmtree(output, ignore_errors=True)
        for dataset in datasets:
            for implementation in ('rust', 'zig'):
                rows = [r for r in results if r['dataset'] == dataset and r['implementation'] == implementation and not r['warmup']]
                print(dataset, implementation, 'median seconds:', statistics.median(r['wall_s'] for r in rows), flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--worker', action='store_true')
    parser.add_argument('--remote')
    parser.add_argument('--root', required=True)
    parser.add_argument('--implementation')
    parser.add_argument('--dataset')
    parser.add_argument('--destination')
    parser.add_argument('--port', type=int)
    parser.add_argument('--rust')
    parser.add_argument('--zig')
    parser.add_argument('--output', default='benchmarks/results.json')
    parser.add_argument('--rust-commit', default=None, help='Optional caller-supplied source revision; binaries are always hash-identified')
    parser.add_argument('--zig-commit', default=None)
    parser.add_argument('--zig-build', default='unspecified prebuilt binary')
    parser.add_argument('--large-mib', type=int, default=128)
    parser.add_argument('--files', type=int, default=1000)
    parser.add_argument('--trials', type=int, default=3)
    args = parser.parse_args()
    worker(args) if args.worker else run(args)
