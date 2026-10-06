#!/usr/bin/env python3
"""Render measured benchmark metadata without assumptions about supplied binaries."""
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
    large_mib = groups['large-file', 'rust'][0]['bytes'] / 1024**2
    count = groups['small-files', 'rust'][0]['bytes'] // 4096
    labels = {'large-file': f'One {large_mib:g} MiB random file', 'small-files': f'{count:,} files × 4 KiB'}
    lines = ['# Rust versus Zig LAN benchmark', '',
             f'Measured against `{data["remote"]}` at {data.get("measured_at", "an unrecorded time (historical result)")}.',
             f'Sender: **{data["sender_cpu"]}**, `{data["sender_platform"]}`.',
             f'Receiver: `{data["receiver_platform"]}`.',
             f'Direction: {data["direction"]}.', '',
             'SSH controls the run; file traffic uses direct TCP rather than SSH.',
             'The harness does not identify whether the route is Wi-Fi or Ethernet.',
             'Fixtures and binaries use an isolated remote `/tmp/xfer-benchmark-*` directory.',
             'No software is installed by the harness.', '', '## Results', '',
             'Medians exclude one warm-up per binary and dataset. Execution order alternates.', '',
             '| Dataset | Rust time | Zig time | Rust MiB/s | Zig MiB/s | Measured trials per binary |',
             '| --- | ---: | ---: | ---: | ---: | ---: |']
    for dataset, label in labels.items():
        trials = len(groups[dataset, 'rust'])
        assert trials == len(groups[dataset, 'zig'])
        lines.append(f'| {label} | {median(dataset,"rust","wall_s"):.3f} s | {median(dataset,"zig","wall_s"):.3f} s | {median(dataset,"rust","mib_s"):.3f} | {median(dataset,"zig","mib_s"):.3f} | {trials} |')
    lines += ['', 'These end-to-end measurements include planning, connection, authentication,',
              'encryption, file I/O and delivery acknowledgement. Receiver/SSH startup,',
              'fixture creation and independent SHA-256 verification are excluded.',
              'LAN conditions and host activity are uncontrolled; this is not a general',
              'language comparison. Browser staging and human approval are not timed.', '',
              '| Dataset | Rust sender CPU | Zig sender CPU | Rust receiver CPU | Zig receiver CPU |',
              '| --- | ---: | ---: | ---: | ---: |']
    for dataset, label in labels.items():
        lines.append(f'| {label} | {median(dataset,"rust","sender_cpu_s"):.3f} s | {median(dataset,"zig","sender_cpu_s"):.3f} s | {median(dataset,"rust","receiver_cpu_s"):.3f} s | {median(dataset,"zig","receiver_cpu_s"):.3f} s |')
    lines += ['', '## Binary provenance', '',
              'Versions below are queried from the actual binaries. SHA-256 hashes and sizes',
              'are recorded for each endpoint in the raw JSON. Source revisions and build',
              'settings are caller-supplied declarations, not inferred from a binary.', '']
    for implementation in ('rust', 'zig'):
        for endpoint in ('sender', 'receiver'):
            binary = data['binaries'][implementation][endpoint]
            lines.append(f'- {implementation.title()} {endpoint}: `{binary.get("version", "version not recorded")}`; SHA-256 `{binary["binary_sha256"]}`.')
        lines.append(f'- Declared {implementation} revision: `{data.get(implementation + "_commit") or "not supplied"}`.')
    lines += [f'- Declared Zig build: `{data.get("zig_build", "not supplied")}`.',
              f'- Zig workspace source fingerprint: `{data["zig_source_sha256"]}` (the workspace, not proof of the prebuilt binary revision).', '',
              f'All **{len(data["results"])} transfers**, including warm-ups, passed independent',
              'SHA-256 inventory comparison before the result was recorded.', '',
              '[results.json](results.json) contains the full measurements and metadata.', '',
              '## Reproduce', '',
              'Provide prebuilt binaries for the local receiver and remote sender. Create',
              'an isolated `/tmp/xfer-benchmark-*` directory remotely and copy the remote',
              'executables there as `rust` and `zig`. Use Python 3 and existing SSH/SCP:', '',
              '```sh', 'python3 scripts/benchmark.py \\', '  --remote user@host \\',
              '  --root /tmp/xfer-benchmark-<isolated-run> \\',
              '  --rust /path/to/local/rust-xfer --zig /path/to/local/zig-xfer \\',
              '  --output benchmarks/results.json', 'python3 scripts/benchmark_report.py', '```', '',
              'Supply `--rust-commit`, `--zig-commit` and `--zig-build` when known. The',
              'sender must reach the receiver LAN address. Remove the specific isolated',
              'remote directory after verification; never use a directory of user files.']
    return '\n'.join(lines) + '\n'


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--input', default='benchmarks/results.json')
    parser.add_argument('--output', default='benchmarks/README.md')
    args = parser.parse_args()
    Path(args.output).write_text(render(json.loads(Path(args.input).read_text())))
