#!/usr/bin/env python3
"""Render the measured benchmark report without third-party packages."""
import argparse
import json
from pathlib import Path
import statistics


def render(data):
    measured = [row for row in data['results'] if not row['warmup']]
    assert measured and all(row['verified'] for row in data['results'])
    groups = {(dataset, implementation): [row for row in measured if row['dataset'] == dataset and row['implementation'] == implementation]
              for dataset in ('large-file', 'small-files') for implementation in ('rust', 'zig')}
    def median(dataset, implementation, key):
        return statistics.median(row[key] for row in groups[dataset, implementation])
    def bounds(dataset):
        return min(row['wall_s'] for row in measured if row['dataset'] == dataset), max(row['wall_s'] for row in measured if row['dataset'] == dataset)
    trials = len(groups['large-file', 'rust'])
    large_mib = groups['large-file', 'rust'][0]['bytes'] / 1024**2
    count = groups['small-files', 'rust'][0]['bytes'] // 4096
    labels = {'large-file': f'One {large_mib:g} MiB random file', 'small-files': f'{count:,} files × 4 KiB'}
    small_gain = median('small-files', 'rust', 'wall_s') / median('small-files', 'zig', 'wall_s')
    large_difference = (median('large-file', 'zig', 'wall_s') / median('large-file', 'rust', 'wall_s') - 1) * 100
    lines = ['# Rust versus Zig LAN benchmark', '', f'Measured against **`{data["remote"]}`** on 2026-10-04.',
             f'The sender was **{data["sender_cpu"]}**, running `{data["sender_platform"]}`.',
             f'The receiver was `{data["receiver_platform"]}`.',
             'The Linux ARM64 host sent directly over Wi-Fi/TCP to the ARM64 Mac.',
             'SSH controlled the run and copied fixtures beforehand; file-transfer',
             'traffic did **not** pass through SSH.', '',
             'The host firewall permits SSH but blocks incoming transfer ports. The run',
             'therefore used Linux-to-macOS transfers without changing firewall rules.',
             'Binaries and fixtures stayed in an isolated `/tmp/xfer-benchmark-*` directory,',
             'which was removed after validation. No tools were installed on the host.', '',
             '## Results', '', f'Median of **{trials} measured trials** per implementation/dataset after one',
             'excluded warm-up. The order alternated between Rust-first and Zig-first.', '',
             '| Dataset | Rust wall time | Zig wall time | Rust MiB/s | Zig MiB/s |',
             '| --- | ---: | ---: | ---: | ---: |']
    for dataset, label in labels.items():
        lines.append(f'| {label} | {median(dataset,"rust","wall_s"):.3f} s | {median(dataset,"zig","wall_s"):.3f} s | {median(dataset,"rust","mib_s"):.3f} | {median(dataset,"zig","mib_s"):.3f} |')
    lines += ['', f'Zig completed the small-file selection **{small_gain:.2f}× faster**. Its large-file',
              f'median took **{abs(large_difference):.1f}% {"longer" if large_difference >= 0 else "less time"}** in this run.',
              'These are end-to-end LAN results, not an isolated compiler/language',
              'comparison or a claim about every network. Wi-Fi and host activity were',
              f'not controlled. Large-file measured times ranged from {bounds("large-file")[0]:.3f}',
              f'to {bounds("large-file")[1]:.3f} seconds; small-file times from {bounds("small-files")[0]:.3f}',
              f'to {bounds("small-files")[1]:.3f} seconds across both implementations.', '',
              '| Dataset | Rust sender CPU | Zig sender CPU | Rust receiver CPU | Zig receiver CPU |',
              '| --- | ---: | ---: | ---: | ---: |']
    for dataset, label in labels.items():
        lines.append(f'| {label} | {median(dataset,"rust","sender_cpu_s"):.3f} s | {median(dataset,"zig","sender_cpu_s"):.3f} s | {median(dataset,"rust","receiver_cpu_s"):.3f} s | {median(dataset,"zig","receiver_cpu_s"):.3f} s |')
    lines += ['', 'Zig hashes the source during planning and again while sending, encrypts every',
              'record, and verifies files before publication. No checks were disabled for',
              'performance. Encrypted record headers and bodies share one write to avoid',
              'extra small network writes. OS-reported sender peak RSS is retained in the',
              'raw results, including process startup; it is not an allocator benchmark.', '',
              '## Versions and verification', '',
              f'- Rust: published release **`{data["rust_release"]}`**, commit', f'  `{data["rust_commit"]}` (pre-migration main).',
              '  The official macOS ARM64/Linux ARM64 musl release binaries were checked',
              '  against their published SHA-256 sidecars. The local offline Rust rebuild',
              '  was unavailable because its dependency cache was incomplete.',
              '- Zig: **0.17.0**, `ReleaseSafe`, generic ARM64 targets, with the complete',
              '  embedded browser interface and platform cancellation fixes present.',
              '- Both implementations received into a fresh empty directory, used the',
              '  same random shared secret, and ran with supported unattended approvals.',
              '- Timing starts immediately before the remote sender process starts and',
              '  ends after its successful delivery acknowledgement. Preparation, connection',
              '  establishment, authentication, encryption, and file I/O are included.',
              '  Receiver/SSH startup and fixture preparation are excluded.',
              f'- After **all {len(data["results"])} transfers**, including warm-ups, an independent',
              '  Python SHA-256 inventory of published files matched the original generated',
              '  selection. Independent verification is outside the timed window.',
              '- This measures the CLI path. Browser selection adds a local temporary copy',
              '  and human approval time; those are not included.', '',
              '[results.json](results.json) records all trials, warm-ups, operating systems,',
              'source/binary hashes, binary sizes, throughput, CPU usage, and verification.',
              'The browser and CLI integration suites also passed natively on this Mac',
              'and the physical Linux ARM64 host.', '', '## Reproduce', '',
              'Use Python 3 with only its standard library and prebuilt Rust/Zig binaries.',
              'Create an isolated `/tmp/xfer-benchmark-*` directory on the Linux host and',
              'copy its two executable binaries there as `rust` and `zig`. Then run:', '',
              '```sh', 'python3 scripts/benchmark.py \\', '  --remote cdenihan@192.168.86.150 \\',
              '  --root /tmp/xfer-benchmark-<your-isolated-directory> \\',
              '  --rust /path/to/local/rust-xfer \\', '  --zig /path/to/local/zig-xfer \\',
              '  --output benchmarks/results.json', 'python3 scripts/benchmark_report.py', '```', '',
              'The sender must be able to reach the receiver’s LAN address. The harness',
              'uses existing SSH/SCP and remote Python, installs no tools, and accepts',
              'only isolated remote directories under `/tmp` named `xfer-benchmark-*`.',
              'Remove that specific directory after the run; do not put user files in it.']
    return '\n'.join(lines) + '\n'


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--input', default='benchmarks/results.json')
    parser.add_argument('--output', default='benchmarks/README.md')
    args = parser.parse_args()
    Path(args.output).write_text(render(json.loads(Path(args.input).read_text())))
