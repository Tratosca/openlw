//! Sockets UDP liées à l'interface Livewire choisie (exigence de la spec : bind sur la NIC).
//!
//! macOS : `IP_BOUND_IF` (via `bind_device_by_index_v4`) ; Linux : `SO_BINDTODEVICE` ; Windows : pas
//! d'équivalent, l'interface est fixée par `IP_MULTICAST_IF` et par l'adhésion aux groupes.
//! Toutes les sockets fixent `IP_MULTICAST_IF` et rejoignent les groupes sur l'IP de l'interface.
//! Ports partagés (`SO_REUSEADDR`) : OpenLW coexiste avec un autre logiciel Livewire sur la machine.

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
    /// Thread d'émission en temps réel (voir `lw_sys::rt::promote`).
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
    // Windows ignore IP_TOS (voir `mark_dscp`) ; ailleurs, un refus est une erreur.
    if let Err(e) = s.set_tos_v4(opts.tos) {
        if !cfg!(windows) {
            return Err(e);
        }
    }
    s.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, src_port).into())?;
    Ok(s.into())
}

/// Socket de réception d'un groupe. macOS, Linux : liée à (groupe, port), le noyau filtre par
/// destination. Windows refuse de lier une adresse multicast : liée à (0.0.0.0, port), la socket ne
/// reçoit que les groupes qu'elle a rejoints sur l'interface.
pub fn rx_socket(
    iface: &Iface,
    group: Ipv4Addr,
    port: u16,
    timeout: Duration,
) -> io::Result<UdpSocket> {
    let s = base_socket(iface)?;
    s.set_recv_buffer_size(1 << 20)?;
    let bind_ip = if cfg!(windows) {
        Ipv4Addr::UNSPECIFIED
    } else {
        group
    };
    s.bind(&SocketAddrV4::new(bind_ip, port).into())?;
    s.join_multicast_v4(&group, &iface.ipv4)?;
    s.set_read_timeout(Some(timeout))?;
    Ok(s.into())
}

/// Marquage DSCP d'un flux émis, actif tant que la valeur vit.
pub struct DscpGuard {
    #[cfg(windows)]
    _flow: lw_sys::qos::Flow,
}

/// Marque les envois de `sock` vers `dest` avec le DSCP de l'octet `tos`. Windows : qWAVE (service
/// ou administrateur requis ; l'erreur est le code Windows). Ailleurs, `IP_TOS` (posé par
/// [`tx_socket`]) suffit : `Ok(None)`.
pub fn mark_dscp(sock: &UdpSocket, dest: SocketAddrV4, tos: u32) -> Result<Option<DscpGuard>, i32> {
    #[cfg(windows)]
    {
        let dscp = u8::try_from(tos >> 2).unwrap_or(63);
        if dscp == 0 {
            return Ok(None);
        }
        lw_sys::qos::set_dscp(sock, dest, dscp).map(|f| Some(DscpGuard { _flow: f }))
    }
    #[cfg(not(windows))]
    {
        let _ = (sock, dest, tos);
        Ok(None)
    }
}
