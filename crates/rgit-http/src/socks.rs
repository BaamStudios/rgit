// SPDX-License-Identifier: MIT
//! SOCKS proxies (`socks4://`, `socks4a://`, `socks5://`, `socks5h://` in
//! `http.proxy`, see `Documentation/config/http.adoc` and `curl(1)`).
//!
//! - SOCKS4 (the protocol description by Ying-Da Lee): `CONNECT` with an
//!   IPv4 address resolved by the client; SOCKS4a sends `0.0.0.1` and the
//!   host name so the proxy resolves it. The user name of the proxy URL is
//!   the user id.
//! - SOCKS5 (RFC 1928) with "no authentication" or user name and password
//!   (RFC 1929); `socks5` sends the resolved address, `socks5h` the host
//!   name.

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};

use crate::error::Transport;
use crate::messages;
use crate::options::{Proxy, ProxyKind};
use crate::trace::Tracer;

/// Resolves a host and port for the client-side resolving variants.
pub(crate) type Resolver<'a> = &'a dyn Fn(&str, u16) -> Result<Vec<SocketAddr>, Transport>;

fn io_err(e: std::io::Error) -> Transport {
    Transport::new(97, format!("SOCKS: {e}"))
}

/// Run the SOCKS handshake on `tcp` so that it carries a connection to
/// `host:port`. `resolve` resolves the target for the variants where the
/// client does that.
pub(crate) fn handshake(
    mut tcp: &TcpStream,
    p: &Proxy,
    host: &str,
    port: u16,
    resolve: Resolver<'_>,
    tracer: &Tracer,
) -> Result<(), Transport> {
    match p.kind {
        ProxyKind::Socks4 | ProxyKind::Socks4a => {
            let mut req = vec![4u8, 1];
            req.extend_from_slice(&port.to_be_bytes());
            let shown = if p.kind == ProxyKind::Socks4 {
                let v4 = resolve(host, port)?
                    .into_iter()
                    .find_map(|a| match a.ip() {
                        IpAddr::V4(ip) => Some(ip),
                        IpAddr::V6(_) => None,
                    })
                    .ok_or_else(|| Transport::new(97, messages::socks4_no_ipv4(host)))?;
                req.extend_from_slice(&v4.octets());
                v4.to_string()
            } else {
                req.extend_from_slice(&[0, 0, 0, 1]);
                host.to_string()
            };
            req.extend_from_slice(p.user.as_deref().unwrap_or("").as_bytes());
            req.push(0);
            if p.kind == ProxyKind::Socks4a {
                req.extend_from_slice(host.as_bytes());
                req.push(0);
            }
            tcp.write_all(&req).map_err(io_err)?;
            let mut reply = [0u8; 8];
            tcp.read_exact(&mut reply).map_err(io_err)?;
            if reply[1] != 0x5a {
                return Err(Transport::new(97, messages::socks4_failed(&shown)));
            }
            tracer.info("SOCKS4 request granted.");
            Ok(())
        }
        ProxyKind::Socks5 | ProxyKind::Socks5h => {
            let with_auth = p.user.is_some();
            let greeting: &[u8] = if with_auth { &[5, 2, 0, 2] } else { &[5, 1, 0] };
            tcp.write_all(greeting).map_err(io_err)?;
            let mut choice = [0u8; 2];
            tcp.read_exact(&mut choice).map_err(io_err)?;
            match choice[1] {
                0 => {}
                2 if with_auth => {
                    let user = p.user.as_deref().unwrap_or("").as_bytes();
                    let pass = p.password.as_deref().unwrap_or("").as_bytes();
                    let mut auth = vec![1u8, user.len().min(255) as u8];
                    auth.extend_from_slice(&user[..user.len().min(255)]);
                    auth.push(pass.len().min(255) as u8);
                    auth.extend_from_slice(&pass[..pass.len().min(255)]);
                    tcp.write_all(&auth).map_err(io_err)?;
                    let mut status = [0u8; 2];
                    tcp.read_exact(&mut status).map_err(io_err)?;
                    if status[1] != 0 {
                        return Err(Transport::new(97, messages::SOCKS5_AUTH_FAILED));
                    }
                }
                _ => return Err(Transport::new(97, messages::SOCKS5_NO_AUTH)),
            }
            let mut req = vec![5u8, 1, 0];
            if p.kind == ProxyKind::Socks5h {
                let h = host.as_bytes();
                req.push(3);
                req.push(h.len().min(255) as u8);
                req.extend_from_slice(&h[..h.len().min(255)]);
            } else {
                let addr = resolve(host, port)?
                    .into_iter()
                    .next()
                    .ok_or_else(|| Transport::new(6, messages::could_not_resolve_host(host)))?;
                match addr.ip() {
                    IpAddr::V4(ip) => {
                        req.push(1);
                        req.extend_from_slice(&ip.octets());
                    }
                    IpAddr::V6(ip) => {
                        req.push(4);
                        req.extend_from_slice(&ip.octets());
                    }
                }
            }
            req.extend_from_slice(&port.to_be_bytes());
            tcp.write_all(&req).map_err(io_err)?;
            let mut head = [0u8; 4];
            tcp.read_exact(&mut head).map_err(io_err)?;
            if head[1] != 0 {
                return Err(Transport::new(97, messages::socks5_failed(host, head[1])));
            }
            // Skip the bound address.
            let rest = match head[3] {
                1 => 4 + 2,
                4 => 16 + 2,
                3 => {
                    let mut n = [0u8; 1];
                    tcp.read_exact(&mut n).map_err(io_err)?;
                    n[0] as usize + 2
                }
                _ => return Err(Transport::new(97, messages::socks5_failed(host, head[1]))),
            };
            let mut skip = vec![0u8; rest];
            tcp.read_exact(&mut skip).map_err(io_err)?;
            tracer.info("SOCKS5 request granted.");
            Ok(())
        }
        ProxyKind::Http | ProxyKind::Https => Ok(()),
    }
}
