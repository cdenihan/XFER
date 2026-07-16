use std::{
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs},
    time::{Duration, Instant},
};

use socket2::{Domain, Protocol, SockRef, Socket, Type};

use crate::error::{Result, XferError};

const IO_TIMEOUT: Duration = Duration::from_secs(120);
const TRANSFER_SOCKET_BUFFER_SIZE: usize = 4 * 1024 * 1024;

pub fn connect(host: &str, port: u16, timeout: Duration) -> Result<TcpStream> {
    connect_with_deadline(host, port, timeout).map(|(stream, _)| stream)
}

pub(crate) fn connect_with_deadline(
    host: &str,
    port: u16,
    timeout: Duration,
) -> Result<(TcpStream, Instant)> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| XferError::invalid_input("connect timeout is too large"))?;
    let addresses = (host, port)
        .to_socket_addrs()
        .map_err(|error| {
            XferError::invalid_input(format!("could not resolve {host}:{port}: {error}"))
        })?
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        return Err(XferError::invalid_input(format!(
            "{host}:{port} did not resolve to an address"
        )));
    }

    let mut last_error = None;
    for address in addresses {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match TcpStream::connect_timeout(&address, remaining.min(Duration::from_secs(5))) {
            Ok(stream) => {
                configure_stream(&stream)?;
                return Ok((stream, deadline));
            }
            Err(error) => last_error = Some((address, error)),
        }
    }

    match last_error {
        Some((address, error)) => Err(XferError::Io(io::Error::new(
            error.kind(),
            format!("could not connect to {address}: {error}"),
        ))),
        None => Err(XferError::Io(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("connection to {host}:{port} timed out"),
        ))),
    }
}

pub fn bind(host: &str, port: u16) -> Result<TcpListener> {
    let ip = host.parse::<IpAddr>().map_err(|error| {
        XferError::invalid_input(format!(
            "bind address {host:?} is not an IP address: {error}"
        ))
    })?;
    let address = SocketAddr::new(ip, port);
    let domain = if address.is_ipv6() {
        Domain::IPV6
    } else {
        Domain::IPV4
    };
    let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
    #[cfg(not(windows))]
    socket.set_reuse_address(true)?;
    if address.is_ipv6() {
        socket.set_only_v6(false)?;
    }
    socket.bind(&address.into())?;
    socket.listen(16)?;
    Ok(socket.into())
}

pub fn configure_stream(stream: &TcpStream) -> Result<()> {
    stream.set_nodelay(true)?;
    restore_read_timeout(stream)?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let socket = SockRef::from(stream);
    let _ = socket.set_recv_buffer_size(TRANSFER_SOCKET_BUFFER_SIZE);
    let _ = socket.set_send_buffer_size(TRANSFER_SOCKET_BUFFER_SIZE);
    Ok(())
}

pub fn suspend_read_timeout(stream: &TcpStream) -> Result<()> {
    stream.set_read_timeout(None)?;
    Ok(())
}

pub fn restore_read_timeout(stream: &TcpStream) -> Result<()> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    Ok(())
}

pub fn restore_io_timeouts(stream: &TcpStream) -> Result<()> {
    restore_read_timeout(stream)?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    Ok(())
}

pub(crate) fn apply_deadline(stream: &TcpStream, deadline: Instant) -> Result<()> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(XferError::Io(io::Error::new(
            io::ErrorKind::TimedOut,
            "connection negotiation timed out",
        )));
    }
    stream.set_read_timeout(Some(remaining))?;
    stream.set_write_timeout(Some(remaining))?;
    Ok(())
}

pub fn local_addresses() -> Result<Vec<IpAddr>> {
    let mut addresses = if_addrs::get_if_addrs()?
        .into_iter()
        .map(|interface| interface.ip())
        .filter(|address| {
            !address.is_loopback()
                && !address.is_unspecified()
                && !address.is_multicast()
                && !matches!(address, IpAddr::V6(address) if address.is_unicast_link_local())
        })
        .collect::<Vec<_>>();
    addresses.sort();
    addresses.dedup();
    Ok(addresses)
}

pub fn local_endpoints(port: u16) -> Result<Vec<SocketAddr>> {
    Ok(endpoints_for_bind(
        IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED),
        port,
        &local_addresses()?,
    ))
}

pub fn listener_endpoints(bind: IpAddr, port: u16) -> Result<Vec<SocketAddr>> {
    Ok(endpoints_for_bind(bind, port, &local_addresses()?))
}

pub(crate) fn endpoints_for_bind(
    bind: IpAddr,
    port: u16,
    local_addresses: &[IpAddr],
) -> Vec<SocketAddr> {
    if !bind.is_unspecified() {
        return vec![SocketAddr::new(bind, port)];
    }
    local_addresses
        .iter()
        .copied()
        .filter(|address| !matches!((bind, address), (IpAddr::V4(_), IpAddr::V6(_))))
        .map(|address| SocketAddr::new(address, port))
        .collect()
}

pub(crate) fn local_ipv4_addresses() -> Result<Vec<Ipv4Addr>> {
    Ok(local_addresses()?
        .into_iter()
        .filter_map(|address| match address {
            IpAddr::V4(address) => Some(address),
            IpAddr::V6(_) => None,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binds_ephemeral_ipv4_port() {
        let listener = bind("127.0.0.1", 0).unwrap();
        assert_ne!(listener.local_addr().unwrap().port(), 0);
    }

    #[test]
    fn endpoint_display_respects_the_listener_bind_family() {
        let addresses = [
            "192.168.1.20".parse().unwrap(),
            "2001:db8::20".parse().unwrap(),
        ];
        assert_eq!(
            endpoints_for_bind("0.0.0.0".parse().unwrap(), 9_000, &addresses),
            vec!["192.168.1.20:9000".parse().unwrap()]
        );
        assert_eq!(
            endpoints_for_bind("::".parse().unwrap(), 9_000, &addresses),
            vec![
                "192.168.1.20:9000".parse().unwrap(),
                "[2001:db8::20]:9000".parse().unwrap()
            ]
        );
        assert_eq!(
            endpoints_for_bind("127.0.0.1".parse().unwrap(), 9_000, &addresses),
            vec!["127.0.0.1:9000".parse().unwrap()]
        );
    }
}
