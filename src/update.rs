pub use rust_cli_release::{UpdateSummary, compare_versions};

use rust_cli_release::ReleaseSpec;

const RELEASE: ReleaseSpec =
    ReleaseSpec::new("xfer", "XFER", "cdenihan/XFER", "XFER", crate::VERSION);

pub fn update_current(
    requested_version: &str,
    quiet_background: bool,
) -> rust_cli_release::Result<UpdateSummary> {
    rust_cli_release::update_current(&RELEASE, requested_version, quiet_background)
}
