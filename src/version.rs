/// Version reported by the CLI.
///
/// The release workflow updates the repository's `VERSION` file before building
/// and `build.rs` exposes that exact public version to the crate.
pub const VERSION: &str = env!("XFER_SOURCE_VERSION");
