use std::path::Path;

pub(crate) fn send(
    receiver_ip: &str,
    path: &Path,
    port: u16,
    excludes: &[String],
    secure: bool,
) -> Result<(), String> {
    if !path.exists() {
        return Err(format!("Path not found: {}", path.display()));
    }

    let (ctrl, data, meta, status) = super::channel_ports(port);
    println!(
        "[INFO] Channels => ctrl:{} data:{} meta:{} status:{} heartbeat:{}",
        ctrl,
        data,
        meta,
        status,
        status.saturating_add(1)
    );

    if path.is_file() {
        super::send_file(receiver_ip, path, port, secure)
    } else if path.is_dir() {
        super::send_dir(receiver_ip, path, port, excludes, secure)
    } else {
        Err(format!("Not a regular file or directory: {}", path.display()))
    }
}
