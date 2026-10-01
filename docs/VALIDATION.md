# Validation and measurements

Local validation ran on macOS ARM64 with Rust 1.98.1. The original baseline passed 114 unit tests and 19 CLI integration tests.

## Release measurements

One sample per workload, using the same disposable insecure loopback harness and filesystem. These figures show observations, not statistically established speedups. CPU time covers both child processes; peak child RSS is cumulative across workloads. GUI compilation does not affect CLI binaries.

| Metric | Before | After |
| --- | ---: | ---: |
| CLI binary bytes | 3,242,416 | 2,661,680 |
| Unique CLI dependency packages, including xfer | 147 | 78 |
| Startup, seconds | 0.00324 | 0.00328 |
| large_file: elapsed seconds | 0.2159 | 0.1841 |
| large_file: child CPU seconds | 0.1836 | 0.1581 |
| large_file: cumulative peak RSS, bytes | 4,685,824 | 4,423,680 |
| small_files: elapsed seconds | 9.6961 | 9.1203 |
| small_files: child CPU seconds | 1.6214 | 1.5134 |
| small_files: cumulative peak RSS, bytes | 5,390,336 | 5,341,184 |
| initial_sync: elapsed seconds | 10.8879 | 9.5912 |
| initial_sync: child CPU seconds | 3.0114 | 2.0340 |
| initial_sync: cumulative peak RSS, bytes | 7,634,944 | 7,487,488 |
| unchanged_sync: elapsed seconds | 0.5613 | 0.5624 |
| unchanged_sync: child CPU seconds | 0.5468 | 0.5379 |
| unchanged_sync: cumulative peak RSS, bytes | 7,634,944 | 7,487,488 |
| shifted_sync: elapsed seconds | 0.7801 | 0.7802 |
| shifted_sync: child CPU seconds | 0.9096 | 0.8755 |
| shifted_sync: cumulative peak RSS, bytes | 7,634,944 | 7,487,488 |

Direct CLI dependencies fell from 25 to 18 (including target-specific libc); the core-only configuration excludes Clap and completion generation too. No build or development dependencies remain in the core manifest. Desktop adds GPUI, with its large separate transitive graph.

Delta signature generation now reuses its buffer. Conflict choices use indexed inventories. Record-write batching was left unchanged: the current results do not justify a transport rewrite. Unchanged and shifted-block timings are effectively equal between samples.

## Checks and limitations

- CLI configuration: 132 unit tests, 19 CLI integration tests, four process/storage tests, and two desktop-worker interoperability tests pass. The core without CLI features passes 130 unit tests.
- Desktop: seven GPUI tests pass, covering Unicode/IME input, all workflow views with collapsed and expanded options at a small window size, preview invalidation, rejection of pending changed-identity trust prompts, stale preview completion, receiver cleanup before retry, mouse focus, Windows clipboard line endings, and preserving preview approval during focus and cursor changes.
- Both configurations pass formatting, Clippy with warnings denied, and release compilation. Local desktop builds use `runtime-shaders`.
- Desktop workers interoperate with CLI processes in both directions, including encrypted transfers, matching security codes, changed-identity rejection/approval, retries, repeated receive sessions, and cancellation during blocked handshake reads.
- POSIX installer rollback/checksum fixtures, two installer-rendering tests, one release-version test, and three packaging tests pass. Archive tests validate platform icons, macOS metadata, Windows contents, checksums, and Linux installation with unusual home-directory paths.
- Workflow YAML syntax passes. PowerShell installer execution is unverified locally; Windows CI includes installer checks.
- The declared core minimum is Rust 1.89; this machine only tested Rust 1.98.1. Desktop requires Rust 1.98 because of the locked dependency graph.
- The app bundle launched and rendered on macOS. Full interactive end-to-end UI and keyboard/accessibility verification remain unverified. It used the optional runtime-shaders feature; this machine lacks Xcode’s optional Metal compiler. Release CI uses precompiled shaders.
- Windows, macOS, and Linux desktop compilation, GPUI tests, release builds, and packaging pass on native CI runners. Windows and Linux interactive GPU rendering and native package launches remain unverified.
- Window close waits asynchronously up to two seconds for cancelled workers to clean up. Native OS quit uses GPUI’s shorter shutdown grace period; blocking DNS and filesystem locks may outlive either grace period.
- Publisher code signing, notarization, and desktop self-update are deferred. macOS bundles receive an ad hoc resource seal, verified locally, so packaged icons remain valid bundle resources.
- The RustSec audit passes in CI. Local cargo-audit and actionlint installation was skipped at the user’s request; actionlint remains unverified. Basic workflow YAML parsing is checked separately.
- GPUI 0.2.2 transitively includes block 0.1.6 and proc-macro-error2 2.0.1, which Rust flags for future compatibility. This change does not patch or fork upstream crates.
