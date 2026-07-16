use std::{
    io,
    net::{IpAddr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs},
    time::{Duration, Instant},
};

use socket2::{Domain, Protocol, Socket, Type};

use crate::error::{Result, XferError};

const IO_TIMEOUT: Duration = Duration::from_secs(120);

pub fn connect(host: &str, port: u16, timeout: Duration) -> Result<TcpStream> {
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

    let started = Instant::now();
    let mut last_error = None;
    for address in addresses {
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            break;
        }
        match TcpStream::connect_timeout(&address, remaining.min(Duration::from_secs(5))) {
            Ok(stream) => {
                configure_stream(&stream)?;
                return Ok(stream);
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

pub fn local_addresses() -> Result<Vec<IpAddr>> {
    let mut addresses = if_addrs::get_if_addrs()?
        .into_iter()
        .map(|interface| interface.ip())
        .filter(|address| !address.is_loopback())
        .collect::<Vec<_>>();
    addresses.sort();
    addresses.dedup();
    Ok(addresses)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binds_ephemeral_ipv4_port() {
        let listener = bind("127.0.0.1", 0).unwrap();
        assert_ne!(listener.local_addr().unwrap().port(), 0);
    }
}
