pub use rust_cli_toolkit::{UpdateSummary, compare_versions};

use rust_cli_toolkit::ReleaseSpec;

const RELEASE: ReleaseSpec =
    ReleaseSpec::new("xfer", "XFER", "cdenihan/XFER", "XFER", crate::VERSION);

pub fn update_current(
    requested_version: &str,
    quiet_background: bool,
) -> rust_cli_toolkit::Result<UpdateSummary> {
    rust_cli_toolkit::update_current(&RELEASE, requested_version, quiet_background)
}
