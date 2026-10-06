"""Liaison des sockets UDP a une interface donnee (macOS et Linux), stdlib uniquement."""
import ipaddress
import re
import socket
import struct
import subprocess
import sys

IP_BOUND_IF = 25  # macOS (netinet/in.h)
SO_BINDTODEVICE = 25  # Linux


def interface_ipv4(name):
    """Premiere adresse IPv4 de l'interface (ifconfig sur macOS, ip sur Linux)."""
    if sys.platform == "darwin":
        out = subprocess.run(["ifconfig", name], capture_output=True, text=True, check=True).stdout
        m = re.search(r"\binet (\d+\.\d+\.\d+\.\d+)", out)
    else:
        out = subprocess.run(["ip", "-4", "-o", "addr", "show", "dev", name], capture_output=True, text=True, check=True).stdout
        m = re.search(r"inet (\d+\.\d+\.\d+\.\d+)", out)
    if not m:
        raise SystemExit(f"aucune adresse IPv4 sur {name}")
    return m.group(1)


def udp_socket(iface, ip=None, ttl=128, tos=0xB8, bind_port=None, reuse=True):
    """Socket UDP liee a l'interface : IP_BOUND_IF / SO_BINDTODEVICE, IP_MULTICAST_IF, TTL, TOS."""
    ip = ip or interface_ipv4(iface)
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_UDP)
    if reuse:
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        if hasattr(socket, "SO_REUSEPORT"):
            s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEPORT, 1)
    if sys.platform == "darwin":
        s.setsockopt(socket.IPPROTO_IP, IP_BOUND_IF, socket.if_nametoindex(iface))
    elif sys.platform.startswith("linux"):
        s.setsockopt(socket.SOL_SOCKET, SO_BINDTODEVICE, iface.encode())
    s.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_IF, socket.inet_aton(ip))
    s.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_TTL, ttl)
    # Bouclage local seulement sur lo0 (essais sur une seule machine) : sinon on s'entendrait soi-même.
    s.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_LOOP, 1 if iface.startswith("lo") else 0)
    s.setsockopt(socket.IPPROTO_IP, socket.IP_TOS, tos)
    if bind_port is not None:
        s.bind(("", bind_port))
    return s, ip


def join(sock, group, ip):
    mreq = struct.pack("4s4s", socket.inet_aton(group), socket.inet_aton(ip))
    sock.setsockopt(socket.IPPROTO_IP, socket.IP_ADD_MEMBERSHIP, mreq)


def is_multicast(addr):
    return ipaddress.IPv4Address(addr).is_multicast
