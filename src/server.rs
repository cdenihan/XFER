use std::path::PathBuf;

pub(crate) fn receive(
    out: Option<PathBuf>,
    port: u16,
    force: bool,
    expected: Option<&str>,
    secure: bool,
) -> Result<(), String> {
    let (ctrl, data, meta, status) = super::channel_ports(port);
    println!(
        "[INFO] Listening channels => ctrl:{} data:{} meta:{} status:{} heartbeat:{}",
        ctrl,
        data,
        meta,
        status,
        status.saturating_add(1)
    );
    super::receive_auto(out, port, force, expected, secure)
}
