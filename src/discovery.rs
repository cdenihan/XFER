use std::{
    collections::BTreeMap,
    env, io,
    net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket},
    sync::mpsc::{self, Receiver, Sender},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, SockAddr, Socket, Type};

use crate::{
    error::{Result, XferError},
    net,
    protocol::VERSION,
};

pub const DISCOVERY_PORT: u16 = 39_090;
pub const PEER_TTL: Duration = Duration::from_secs(7);

const DISCOVERY_GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 90, 90);
const DISCOVERY_INTERVAL: Duration = Duration::from_secs(2);
const LISTENER_POLL_INTERVAL: Duration = Duration::from_millis(500);
const MAX_ANNOUNCEMENT_SIZE: usize = 1_024;
const SERVICE_NAME: &str = "xfer";
const DISCOVERY_VERSION: u8 = 1;
const MAX_MACHINE_NAME_CHARS: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DiscoveredPeer {
    pub name: String,
    pub address: SocketAddr,
    pub secure: bool,
}

#[derive(Debug, Deserialize, Serialize)]
struct Announcement {
    service: String,
    discovery_version: u8,
    protocol_version: u16,
    name: String,
    port: u16,
    secure: bool,
}

pub struct Advertiser {
    stop: Sender<()>,
    handle: Option<JoinHandle<()>>,
}

impl Advertiser {
    pub fn start(port: u16, secure: bool, bind: IpAddr) -> Result<Self> {
        if port == 0 {
            return Err(XferError::invalid_input(
                "discovery requires a non-zero transfer port",
            ));
        }
        let announcement = Announcement {
            service: SERVICE_NAME.into(),
            discovery_version: DISCOVERY_VERSION,
            protocol_version: VERSION,
            name: machine_name(),
            port,
            secure,
        };
        let payload = serde_json::to_vec(&announcement)
            .map_err(|error| XferError::Serialization(error.to_string()))?;
        let socket = multicast_sender()?;
        let interfaces = discovery_interfaces(bind)?;
        let destination = SockAddr::from(SocketAddrV4::new(DISCOVERY_GROUP, DISCOVERY_PORT));
        announce(&socket, &interfaces, &destination, &payload)?;
        let (stop, stop_rx) = mpsc::channel();
        let handle = thread::Builder::new()
            .name("xfer-discovery-advertiser".into())
            .spawn(move || {
                loop {
                    let _ = announce(&socket, &interfaces, &destination, &payload);
                    match stop_rx.recv_timeout(DISCOVERY_INTERVAL) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                }
            })?;
        Ok(Self {
            stop,
            handle: Some(handle),
        })
    }
}

impl Drop for Advertiser {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub struct Browser {
    peers: Receiver<DiscoveredPeer>,
    stop: Sender<()>,
    handle: Option<JoinHandle<()>>,
}

impl Browser {
    pub fn start() -> Result<Self> {
        let socket = multicast_listener()?;
        let (peer_tx, peers) = mpsc::channel();
        let (stop, stop_rx) = mpsc::channel();
        let handle = thread::Builder::new()
            .name("xfer-discovery-browser".into())
            .spawn(move || browse(socket, &peer_tx, &stop_rx))?;
        Ok(Self {
            peers,
            stop,
            handle: Some(handle),
        })
    }

    pub fn try_recv(&self) -> Option<DiscoveredPeer> {
        self.peers.try_recv().ok()
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub fn group_address() -> SocketAddr {
    SocketAddrV4::new(DISCOVERY_GROUP, DISCOVERY_PORT).into()
}

pub fn discover_for(timeout: Duration) -> Result<Vec<DiscoveredPeer>> {
    let browser = Browser::start()?;
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| XferError::invalid_input("discovery timeout is too large"))?;
    let mut peers = BTreeMap::new();
    while Instant::now() < deadline {
        while let Some(peer) = browser.try_recv() {
            peers.insert(peer.address, peer);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        thread::sleep(remaining.min(Duration::from_millis(50)));
    }
    while let Some(peer) = browser.try_recv() {
        peers.insert(peer.address, peer);
    }
    Ok(peers.into_values().collect())
}

fn multicast_sender() -> Result<Socket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_multicast_ttl_v4(1)?;
    socket.set_multicast_loop_v4(true)?;
    socket.bind(&SockAddr::from(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)))?;
    Ok(socket)
}

fn multicast_listener() -> Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    let _ = socket.set_reuse_port(true);
    socket.bind(&SockAddr::from(SocketAddrV4::new(
        Ipv4Addr::UNSPECIFIED,
        DISCOVERY_PORT,
    )))?;

    let interfaces = net::local_ipv4_addresses()?;
    let join_interfaces = if interfaces.is_empty() {
        vec![Ipv4Addr::UNSPECIFIED]
    } else {
        interfaces
    };
    let mut joined = false;
    let mut last_error = None;
    for interface in join_interfaces {
        match socket.join_multicast_v4(&DISCOVERY_GROUP, &interface) {
            Ok(()) => joined = true,
            Err(error) => last_error = Some(error),
        }
    }
    if !joined {
        return Err(last_error
            .unwrap_or_else(|| io::Error::other("no IPv4 interface accepted multicast"))
            .into());
    }

    let socket: UdpSocket = socket.into();
    socket.set_read_timeout(Some(LISTENER_POLL_INTERVAL))?;
    Ok(socket)
}

fn discovery_interfaces(bind: IpAddr) -> Result<Vec<Ipv4Addr>> {
    match bind {
        IpAddr::V4(address) if address.is_loopback() => Err(XferError::invalid_input(
            "a loopback-only receiver cannot be advertised to the LAN",
        )),
        IpAddr::V6(address) if !address.is_unspecified() => Err(XferError::invalid_input(
            "IPv4 multicast discovery cannot advertise an IPv6-only bind",
        )),
        _ => {
            let interfaces = net::local_ipv4_addresses()?;
            select_discovery_interfaces(bind, interfaces)
        }
    }
}

fn select_discovery_interfaces(bind: IpAddr, interfaces: Vec<Ipv4Addr>) -> Result<Vec<Ipv4Addr>> {
    match bind {
        IpAddr::V4(address) if address.is_unspecified() => Ok(interfaces),
        IpAddr::V4(address) if interfaces.contains(&address) => Ok(vec![address]),
        IpAddr::V4(address) => Err(XferError::invalid_input(format!(
            "{address} is not an active local IPv4 interface"
        ))),
        IpAddr::V6(address) if address.is_unspecified() => Ok(interfaces),
        IpAddr::V6(_) => Err(XferError::invalid_input(
            "IPv4 multicast discovery cannot advertise an IPv6-only bind",
        )),
    }
}

fn announce(
    socket: &Socket,
    interfaces: &[Ipv4Addr],
    destination: &SockAddr,
    payload: &[u8],
) -> io::Result<()> {
    if interfaces.is_empty() {
        socket.set_multicast_if_v4(&Ipv4Addr::UNSPECIFIED)?;
        socket.send_to(payload, destination)?;
        return Ok(());
    }
    let mut sent = false;
    let mut last_error = None;
    for interface in interfaces {
        if let Err(error) = socket.set_multicast_if_v4(interface) {
            last_error = Some(error);
            continue;
        }
        match socket.send_to(payload, destination) {
            Ok(_) => sent = true,
            Err(error) => last_error = Some(error),
        }
    }
    if sent {
        Ok(())
    } else {
        Err(last_error.unwrap_or_else(|| io::Error::other("no interface sent the announcement")))
    }
}

fn browse(socket: UdpSocket, peers: &Sender<DiscoveredPeer>, stop: &Receiver<()>) {
    let mut buffer = [0_u8; MAX_ANNOUNCEMENT_SIZE];
    loop {
        if stop.try_recv().is_ok() {
            return;
        }
        match socket.recv_from(&mut buffer) {
            Ok((length, source)) => {
                if let Some(peer) = decode_announcement(&buffer[..length], source)
                    && peers.send(peer).is_err()
                {
                    return;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return,
        }
    }
}

fn decode_announcement(payload: &[u8], source: SocketAddr) -> Option<DiscoveredPeer> {
    let announcement: Announcement = serde_json::from_slice(payload).ok()?;
    if announcement.service != SERVICE_NAME
        || announcement.discovery_version != DISCOVERY_VERSION
        || announcement.protocol_version != VERSION
        || announcement.port == 0
        || source.ip().is_unspecified()
        || source.ip().is_multicast()
        || !valid_machine_name(&announcement.name)
    {
        return None;
    }
    Some(DiscoveredPeer {
        name: announcement.name,
        address: SocketAddr::new(source.ip(), announcement.port),
        secure: announcement.secure,
    })
}

fn machine_name() -> String {
    ["XFER_NAME", "COMPUTERNAME", "HOSTNAME"]
        .into_iter()
        .filter_map(|variable| env::var(variable).ok())
        .map(|value| sanitize_machine_name(&value))
        .find(|value| !value.is_empty())
        .unwrap_or_else(|| "XFER receiver".into())
}

fn sanitize_machine_name(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_MACHINE_NAME_CHARS)
        .collect()
}

fn valid_machine_name(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= MAX_MACHINE_NAME_CHARS
        && value.chars().all(|character| !character.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announcement_round_trips_source_address_and_port() {
        let payload = serde_json::to_vec(&Announcement {
            service: SERVICE_NAME.into(),
            discovery_version: DISCOVERY_VERSION,
            protocol_version: VERSION,
            name: "studio-mac".into(),
            port: 9_123,
            secure: true,
        })
        .unwrap();
        let peer = decode_announcement(&payload, "192.168.1.20:45000".parse().unwrap()).unwrap();
        assert_eq!(peer.name, "studio-mac");
        assert_eq!(peer.address, "192.168.1.20:9123".parse().unwrap());
        assert!(peer.secure);
    }

    #[test]
    fn announcement_rejects_other_protocols_and_invalid_names() {
        let payload = serde_json::to_vec(&Announcement {
            service: SERVICE_NAME.into(),
            discovery_version: DISCOVERY_VERSION,
            protocol_version: VERSION + 1,
            name: "other".into(),
            port: DEFAULT_TEST_PORT,
            secure: true,
        })
        .unwrap();
        assert!(decode_announcement(&payload, "192.168.1.20:45000".parse().unwrap()).is_none());

        let payload = serde_json::to_vec(&Announcement {
            service: SERVICE_NAME.into(),
            discovery_version: DISCOVERY_VERSION,
            protocol_version: VERSION,
            name: "bad\nname".into(),
            port: DEFAULT_TEST_PORT,
            secure: true,
        })
        .unwrap();
        assert!(decode_announcement(&payload, "192.168.1.20:45000".parse().unwrap()).is_none());
    }

    #[test]
    fn interface_selection_honors_specific_and_dual_stack_binds() {
        let interfaces = vec![Ipv4Addr::new(10, 0, 0, 2), Ipv4Addr::new(192, 168, 1, 20)];
        assert_eq!(
            select_discovery_interfaces("192.168.1.20".parse().unwrap(), interfaces.clone())
                .unwrap(),
            vec![Ipv4Addr::new(192, 168, 1, 20)]
        );
        assert_eq!(
            select_discovery_interfaces("::".parse().unwrap(), interfaces.clone()).unwrap(),
            interfaces
        );
        assert!(
            select_discovery_interfaces(
                "192.168.2.20".parse().unwrap(),
                vec![Ipv4Addr::new(192, 168, 1, 20)]
            )
            .is_err()
        );
    }

    #[test]
    fn announcement_rejects_wrong_service_port_and_source_addresses() {
        let announcement = |service: &str, port: u16| {
            serde_json::to_vec(&Announcement {
                service: service.into(),
                discovery_version: DISCOVERY_VERSION,
                protocol_version: VERSION,
                name: "receiver".into(),
                port,
                secure: true,
            })
            .unwrap()
        };
        assert!(
            decode_announcement(
                &announcement("other", DEFAULT_TEST_PORT),
                "192.168.1.20:45000".parse().unwrap()
            )
            .is_none()
        );
        assert!(
            decode_announcement(
                &announcement(SERVICE_NAME, 0),
                "192.168.1.20:45000".parse().unwrap()
            )
            .is_none()
        );
        assert!(
            decode_announcement(
                &announcement(SERVICE_NAME, DEFAULT_TEST_PORT),
                "0.0.0.0:45000".parse().unwrap()
            )
            .is_none()
        );
        assert!(
            decode_announcement(
                &announcement(SERVICE_NAME, DEFAULT_TEST_PORT),
                "239.1.1.1:45000".parse().unwrap()
            )
            .is_none()
        );
    }

    #[test]
    fn machine_name_sanitization_trims_controls_and_length() {
        let sanitized = sanitize_machine_name(&format!(
            "  receiver\n{}  ",
            "x".repeat(MAX_MACHINE_NAME_CHARS * 2)
        ));
        assert!(!sanitized.contains('\n'));
        assert_eq!(sanitized.chars().count(), MAX_MACHINE_NAME_CHARS);
        assert!(valid_machine_name(&sanitized));
        assert!(!valid_machine_name(""));
    }

    #[test]
    fn discovery_group_is_administratively_scoped_ipv4() {
        assert_eq!(group_address().port(), DISCOVERY_PORT);
        assert_eq!(group_address().ip(), IpAddr::V4(DISCOVERY_GROUP));
        assert!(group_address().ip().is_multicast());
    }

    const DEFAULT_TEST_PORT: u16 = 9_000;
}
