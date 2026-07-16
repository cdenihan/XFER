/// Version reported by the CLI.
///
/// Release builds inject `XFER_RELEASE_VERSION` at compile time. Local builds
/// fall back to the package version from `Cargo.toml`.
pub const VERSION: &str = match option_env!("XFER_RELEASE_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};
