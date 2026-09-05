/// Version reported by the CLI.
///
/// The release workflow updates the repository's `VERSION` file before building
/// and `build.rs` exposes that exact public version to the crate.
pub const VERSION: &str = env!("XFER_SOURCE_VERSION");

use crate::error::{Result, XferError};

pub(crate) fn validate_peer_release_version(version: Option<&str>) -> Result<()> {
    let Some(version) = version else {
        return Ok(());
    };
    if version.is_empty()
        || version.len() > 64
        || !version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err(XferError::protocol("peer sent an invalid release version"));
    }
    Ok(())
}
