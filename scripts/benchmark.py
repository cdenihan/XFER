#!/usr/bin/env python3
"""Repeatable local workloads; uses loopback, isolated identities and disposable data."""
import argparse, json, os, pathlib, resource, socket, subprocess, tempfile, time

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('binary', type=pathlib.Path)
    parser.add_argument('--output', type=pathlib.Path, required=True)
    args = parser.parse_args()
    binary = str(args.binary.resolve())
    results = {'binary_bytes': os.path.getsize(binary), 'workloads': {}}
    with tempfile.TemporaryDirectory(prefix='xfer-bench-') as temporary:
        root = pathlib.Path(temporary)
        source = root / 'source'; source.mkdir()
        (source / 'large.bin').write_bytes(bytes(range(256)) * (1024 * 512))
        small = source / 'small'; small.mkdir()
        for number in range(2000):
            (small / f'{number:05}.txt').write_bytes(b'payload' * 100)
        output = root / 'output'; output.mkdir()
        config = root / 'config'
        for name, path, syncing in [('large_file', source / 'large.bin', False), ('small_files', small, False), ('initial_sync', source, True), ('unchanged_sync', source, True), ('shifted_sync', source, True)]:
            if name == 'shifted_sync':
                file = source / 'large.bin'; file.write_bytes(b'inserted prefix' + file.read_bytes())
            with socket.socket() as probe:
                probe.bind(('127.0.0.1', 0)); port = probe.getsockname()[1]
            server_args = [binary, '--config-dir', str(config), 'receive', '--bind', '127.0.0.1', '--port', str(port), '--output', str(output), '--no-discovery', '--insecure']
            if syncing: server_args += ['--sync', '--once']
            server = subprocess.Popen(server_args, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
            try:
                time.sleep(0.15)
                before = resource.getrusage(resource.RUSAGE_CHILDREN)
                start = time.perf_counter()
                completed = subprocess.run([binary, '--json', '--config-dir', str(config), 'sync' if syncing else 'send', '127.0.0.1', str(path), '--port', str(port), '--insecure'], capture_output=True, check=True, timeout=120)
                _, error = server.communicate(timeout=30)
                if server.returncode: raise RuntimeError(error.decode())
                elapsed = time.perf_counter() - start
            finally:
                if server.poll() is None:
                    server.kill()
                server.wait()
                server.stderr.close()
            after = resource.getrusage(resource.RUSAGE_CHILDREN)
            final = json.loads(completed.stdout.splitlines()[-1])
            payload = final['sync']['sent_bytes'] if final.get('sync') else final['total_bytes']
            results['workloads'][name] = {'seconds': elapsed, 'payload_bytes': payload, 'bytes_per_second': payload / elapsed, 'child_cpu_seconds': after.ru_utime + after.ru_stime - before.ru_utime - before.ru_stime, 'max_child_rss': after.ru_maxrss}
        start = time.perf_counter()
        for _ in range(20): subprocess.run([binary, '--version'], stdout=subprocess.DEVNULL, check=True)
        results['startup_seconds'] = (time.perf_counter() - start) / 20
    args.output.write_text(json.dumps(results, indent=2) + '\n')

if __name__ == '__main__': main()
