fn main() {
    rust_cli_toolkit::emit_version_file(
        "VERSION",
        "XFER_SOURCE_VERSION",
        rust_cli_toolkit::VersionFormat::Calendar,
    )
    .expect("VERSION must use YYYY.MM.DD.N format");
}
