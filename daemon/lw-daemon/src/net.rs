//! Sockets UDP liées à l'interface Livewire choisie (exigence de la spec : bind sur la NIC).
//!
//! macOS : `IP_BOUND_IF` (via `bind_device_by_index_v4`) ; Linux : `SO_BINDTODEVICE`.
//! Toutes les sockets fixent aussi `IP_MULTICAST_IF` et rejoignent les groupes sur l'IP de l'interface.

use std::io;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::time::Duration;

use socket2::{Domain, Protocol, Socket, Type};

use crate::iface::Iface;

/// Paramètres d'émission.
#[derive(Debug, Clone, Copy)]
pub struct TxOptions {
    pub ttl: u32,
    /// Octet TOS (DSCP × 4). EF = 0xB8 (défaut usuel Livewire), AF41 = 0x88 (recommandé AES67).
    pub tos: u32,
    /// Thread d'émission en temps réel (`THREAD_TIME_CONSTRAINT_POLICY`).
    pub realtime: bool,
}

impl Default for TxOptions {
    fn default() -> Self {
        Self {
            ttl: 128,
            tos: 0xB8,
            realtime: true,
        }
    }
}

fn base_socket(iface: &Iface) -> io::Result<Socket> {
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    s.set_reuse_address(true)?;
    #[cfg(unix)]
    s.set_reuse_port(true)?;
    bind_to_iface(&s, iface)?;
    s.set_multicast_if_v4(&iface.ipv4)?;
    // Sur lo0 (tests), le bouclage multicast est indispensable ; sur une vraie NIC on ne veut pas
    // recevoir nos propres flux.
    s.set_multicast_loop_v4(iface.loopback)?;
    Ok(s)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn bind_to_iface(s: &Socket, iface: &Iface) -> io::Result<()> {
    s.bind_device_by_index_v4(iface.index_nz())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn bind_to_iface(_s: &Socket, _iface: &Iface) -> io::Result<()> {
    Ok(())
}

/// Socket d'émission. `src_port` : port source (OpenLW émet avec port source = port destination).
pub fn tx_socket(iface: &Iface, src_port: u16, opts: TxOptions) -> io::Result<UdpSocket> {
    let s = base_socket(iface)?;
    s.set_multicast_ttl_v4(opts.ttl)?;
    s.set_tos_v4(opts.tos)?;
    s.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, src_port).into())?;
    Ok(s.into())
}

/// Socket de réception d'un groupe : liée à (groupe, port) pour que le noyau filtre par destination.
pub fn rx_socket(
    iface: &Iface,
    group: Ipv4Addr,
    port: u16,
    timeout: Duration,
) -> io::Result<UdpSocket> {
    let s = base_socket(iface)?;
    s.set_recv_buffer_size(1 << 20)?;
    s.bind(&SocketAddrV4::new(group, port).into())?;
    s.join_multicast_v4(&group, &iface.ipv4)?;
    s.set_read_timeout(Some(timeout))?;
    Ok(s.into())
}
