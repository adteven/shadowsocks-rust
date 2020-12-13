//! Shadowsocks manager connecting interface

use std::{
    fmt,
    io::{self, ErrorKind},
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
};

use tokio::net::UdpSocket;
#[cfg(unix)]
use tokio::net::{unix::SocketAddr as UnixSocketAddr, UnixDatagram};

use crate::{config::ManagerAddr, context::Context, relay::sys::create_udp_socket};

#[derive(Debug)]
pub enum ManagerSocketAddr {
    SocketAddr(SocketAddr),
    #[cfg(unix)]
    UnixSocketAddr(UnixSocketAddr),
}

impl ManagerSocketAddr {
    /// Check if it is unnamed (not binded to any valid address), only valid for `UnixSocketAddr`
    pub fn is_unnamed(&self) -> bool {
        match *self {
            ManagerSocketAddr::SocketAddr(..) => false,
            #[cfg(unix)]
            ManagerSocketAddr::UnixSocketAddr(ref s) => s.is_unnamed(),
        }
    }
}

impl fmt::Display for ManagerSocketAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            ManagerSocketAddr::SocketAddr(ref saddr) => fmt::Display::fmt(saddr, f),
            #[cfg(unix)]
            ManagerSocketAddr::UnixSocketAddr(ref saddr) => fmt::Debug::fmt(saddr, f),
        }
    }
}

/// Datagram socket for manager
///
/// For *nix system, this is a wrapper for both UDP socket and Unix socket
pub enum ManagerDatagram {
    UdpDatagram(UdpSocket),
    #[cfg(unix)]
    UnixDatagram(UnixDatagram),
}

impl ManagerDatagram {
    /// Create a `ManagerDatagram` binding to requested `bind_addr`
    pub async fn bind(context: &Context, bind_addr: &ManagerAddr) -> io::Result<ManagerDatagram> {
        match *bind_addr {
            ManagerAddr::SocketAddr(ref saddr) => Ok(ManagerDatagram::UdpDatagram(create_udp_socket(saddr).await?)),
            ManagerAddr::DomainName(ref dname, port) => {
                let (_, socket) = lookup_then!(context, dname, port, |saddr| { create_udp_socket(&saddr).await })?;

                Ok(ManagerDatagram::UdpDatagram(socket))
            }
            #[cfg(unix)]
            ManagerAddr::UnixSocketAddr(ref path) => {
                use std::fs;

                // Remove it first incase it is already exists
                let _ = fs::remove_file(path);

                Ok(ManagerDatagram::UnixDatagram(UnixDatagram::bind(path)?))
            }
        }
    }

    /// Create a `ManagerDatagram` for sending data to manager
    pub async fn connect(context: &Context, bind_addr: &ManagerAddr) -> io::Result<ManagerDatagram> {
        match *bind_addr {
            ManagerAddr::SocketAddr(sa) => ManagerDatagram::connect_socket_addr(sa).await,

            ManagerAddr::DomainName(ref dname, port) => {
                // Try connect to all socket addresses
                lookup_then!(context, dname, port, |addr| {
                    ManagerDatagram::connect_socket_addr(addr).await
                })
                .map(|(_, d)| d)
            }

            #[cfg(unix)]
            // For unix socket, it doesn't need to bind to any valid address
            // Because manager won't response to you
            ManagerAddr::UnixSocketAddr(..) => Ok(ManagerDatagram::UnixDatagram(UnixDatagram::unbound()?)),
        }
    }

    async fn connect_socket_addr(sa: SocketAddr) -> io::Result<ManagerDatagram> {
        let socket = match sa {
            SocketAddr::V4(..) => {
                // Bind to 0.0.0.0 and let system allocate a port
                let local_addr = SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0);
                create_udp_socket(&local_addr).await?
            }
            SocketAddr::V6(..) => {
                // Bind to :: and let system allocate a port
                let local_addr = SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), 0);
                create_udp_socket(&local_addr).await?
            }
        };

        socket.connect(sa).await?;

        Ok(ManagerDatagram::UdpDatagram(socket))
    }

    /// Receives data from the socket.
    pub async fn recv(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match *self {
            ManagerDatagram::UdpDatagram(ref mut udp) => udp.recv(buf).await,
            #[cfg(unix)]
            ManagerDatagram::UnixDatagram(ref mut unix) => unix.recv(buf).await,
        }
    }

    /// Receives data from the socket.
    pub async fn recv_from(&mut self, buf: &mut [u8]) -> io::Result<(usize, ManagerSocketAddr)> {
        match *self {
            ManagerDatagram::UdpDatagram(ref mut udp) => {
                let (s, addr) = udp.recv_from(buf).await?;
                Ok((s, ManagerSocketAddr::SocketAddr(addr)))
            }
            #[cfg(unix)]
            ManagerDatagram::UnixDatagram(ref mut unix) => {
                let (s, addr) = unix.recv_from(buf).await?;
                Ok((s, ManagerSocketAddr::UnixSocketAddr(addr)))
            }
        }
    }

    /// Sends data to the socket
    pub async fn send(&mut self, buf: &[u8]) -> io::Result<usize> {
        match *self {
            ManagerDatagram::UdpDatagram(ref mut udp) => udp.send(buf).await,
            #[cfg(unix)]
            ManagerDatagram::UnixDatagram(ref mut unix) => unix.send(buf).await,
        }
    }

    /// Sends data to the socket to the specified address.
    pub async fn send_to(&mut self, buf: &[u8], target: &ManagerSocketAddr) -> io::Result<usize> {
        match *self {
            ManagerDatagram::UdpDatagram(ref mut udp) => match *target {
                ManagerSocketAddr::SocketAddr(ref saddr) => udp.send_to(buf, saddr).await,
                #[cfg(unix)]
                ManagerSocketAddr::UnixSocketAddr(..) => {
                    let err = io::Error::new(ErrorKind::InvalidInput, "udp datagram requires IP address target");
                    Err(err)
                }
            },
            #[cfg(unix)]
            ManagerDatagram::UnixDatagram(ref mut unix) => match *target {
                ManagerSocketAddr::UnixSocketAddr(ref saddr) => match saddr.as_pathname() {
                    Some(paddr) => unix.send_to(buf, paddr).await,
                    None => {
                        let err = io::Error::new(ErrorKind::InvalidInput, "target address must not be unnamed");
                        Err(err)
                    }
                },
                ManagerSocketAddr::SocketAddr(..) => {
                    let err = io::Error::new(ErrorKind::InvalidInput, "unix datagram requires path address target");
                    Err(err)
                }
            },
        }
    }

    /// Sends data on the socket to the specified manager address
    pub async fn send_to_manager(&mut self, buf: &[u8], context: &Context, target: &ManagerAddr) -> io::Result<usize> {
        match *self {
            ManagerDatagram::UdpDatagram(ref mut udp) => match *target {
                ManagerAddr::SocketAddr(ref saddr) => udp.send_to(buf, saddr).await,
                ManagerAddr::DomainName(ref dname, port) => {
                    let (_, n) = lookup_then!(context, dname, port, |saddr| { udp.send_to(buf, saddr).await })?;
                    Ok(n)
                }
                #[cfg(unix)]
                ManagerAddr::UnixSocketAddr(..) => {
                    let err = io::Error::new(ErrorKind::InvalidInput, "udp datagram requires IP address target");
                    Err(err)
                }
            },
            #[cfg(unix)]
            ManagerDatagram::UnixDatagram(ref mut unix) => match *target {
                ManagerAddr::UnixSocketAddr(ref paddr) => unix.send_to(buf, paddr).await,
                ManagerAddr::SocketAddr(..) | ManagerAddr::DomainName(..) => {
                    let err = io::Error::new(ErrorKind::InvalidInput, "unix datagram requires path address target");
                    Err(err)
                }
            },
        }
    }

    /// Returns the local address that this socket is bound to.
    pub fn local_addr(&self) -> io::Result<ManagerSocketAddr> {
        match *self {
            ManagerDatagram::UdpDatagram(ref socket) => socket.local_addr().map(ManagerSocketAddr::SocketAddr),
            #[cfg(unix)]
            ManagerDatagram::UnixDatagram(ref dgram) => dgram.local_addr().map(ManagerSocketAddr::UnixSocketAddr),
        }
    }
}
