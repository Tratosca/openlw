//! UDP sockets bound to the selected Livewire interface (specification requirement: bind to NIC).
//!
//! macOS: `IP_BOUND_IF` (through `bind_device_by_index_v4`); Linux: `SO_BINDTODEVICE`; Windows: no
//! equivalent; interface selected through `IP_MULTICAST_IF` and group membership.
//! All sockets set `IP_MULTICAST_IF` and join groups on the interface IP.
//! Shared ports (`SO_REUSEADDR`): OpenLW coexists with other Livewire software on the machine.

use std::io;
use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::time::Duration;

use socket2::{Domain, Protocol, Socket, Type};

use crate::iface::Iface;

/// Transmission parameters.
#[derive(Debug, Clone, Copy)]
pub struct TxOptions {
    pub ttl: u32,
    /// TOS byte (DSCP × 4). EF = 0xB8 (usual Livewire default), AF41 = 0x88 (AES67 recommendation).
    pub tos: u32,
    /// Real-time transmit thread (see `lw_sys::rt::promote`).
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
    // On lo0 (tests), multicast loopback is essential; on a physical NIC we do not want to
    // receive our own streams.
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

/// Transmit socket. `src_port`: source port (OpenLW uses source port = destination port).
pub fn tx_socket(iface: &Iface, src_port: u16, opts: TxOptions) -> io::Result<UdpSocket> {
    let s = base_socket(iface)?;
    s.set_multicast_ttl_v4(opts.ttl)?;
    // Windows ignores IP_TOS (see `mark_dscp`); elsewhere, rejection is an error.
    if let Err(e) = s.set_tos_v4(opts.tos) {
        if !cfg!(windows) {
            return Err(e);
        }
    }
    s.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, src_port).into())?;
    Ok(s.into())
}

/// Group receive socket. macOS/Linux: bound to (group, port), with kernel filtering by
/// destination. Windows rejects binding multicast addresses: bound to (0.0.0.0, port), the socket
/// receives only groups joined on the interface.
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

/// DSCP marking for a transmitted stream, active while the value lives.
pub struct DscpGuard {
    #[cfg(windows)]
    _flow: lw_sys::qos::Flow,
}

/// Mark sends from `sock` to `dest` with the DSCP from `tos`. Windows: qWAVE (service
/// or administrator required; error is a Windows code). Elsewhere, `IP_TOS` (set by
/// [`tx_socket`]) suffices: `Ok(None)`.
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
