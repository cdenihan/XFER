use std::{
    fs,
    io::{Error, ErrorKind, Result},
};

fn cargo_version(public_version: &str) -> Result<String> {
    let parts = public_version.split('.').collect::<Vec<_>>();
    if parts.len() != 4
        || parts[0].len() != 4
        || parts[1].len() != 2
        || parts[2].len() != 2
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "calendar versions must use YYYY.MM.DD.N",
        ));
    }
    let year = parts[0].parse::<u16>().map_err(invalid_number)?;
    let month = parts[1].parse::<u8>().map_err(invalid_number)?;
    let day = parts[2].parse::<u8>().map_err(invalid_number)?;
    let release = parts[3].parse::<u32>().map_err(invalid_number)?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || release == 0 {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "calendar version contains an invalid date or release number",
        ));
    }
    Ok(format!("{year}.{month}.{day}-{release}"))
}

fn invalid_number(error: impl std::fmt::Display) -> Error {
    Error::new(
        ErrorKind::InvalidInput,
        format!("invalid numeric version component: {error}"),
    )
}

fn main() -> Result<()> {
    println!("cargo:rerun-if-changed=VERSION");
    let version = fs::read_to_string("VERSION")?;
    let version = version.trim();
    cargo_version(version)?;
    println!("cargo:rustc-env=XFER_SOURCE_VERSION={version}");
    Ok(())
}
