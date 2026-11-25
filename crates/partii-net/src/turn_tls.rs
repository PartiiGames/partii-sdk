//! TURN over TLS for the native driver.
//!
//! webrtc-rs reaches TURN servers over UDP only, so on a network that blocks
//! UDP (offices, schools, hotel wifi) a native client had no TURN path and
//! played on the ws relay. This bridges it: webrtc-rs talks ordinary TURN over
//! UDP to `127.0.0.1:<port>`, and every datagram goes to the real server over
//! TLS (the `turns:` URL, port 443 on Cloudflare), and back. Over a stream the
//! messages are the same STUN and ChannelData messages, back to back
//! (RFC 8656 §3.1, §12.5: ChannelData padded to 4 bytes), so the bridge only
//! frames them; credentials and integrity are end to end.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio_rustls::rustls::{self, pki_types::ServerName};

/// A running bridge; dropping it closes the TLS connection.
pub(crate) struct Bridge {
    /// What webrtc-rs is told: `turn:127.0.0.1:<port>?transport=udp`.
    pub url: String,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// `turns:host:port?transport=tcp` → (host, port).
pub(crate) fn parse_turns(url: &str) -> Option<(String, u16)> {
    let rest = url.strip_prefix("turns:")?;
    let hostport = rest.split('?').next()?;
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().ok()?),
        None => (hostport, 5349),
    };
    (!host.is_empty()).then(|| (host.to_string(), port))
}

/// The length of the first whole STUN or ChannelData message in `buf`
/// (with its padding), `Ok(None)` if more bytes are needed, `Err` if the
/// stream holds neither.
pub(crate) fn frame_len(buf: &[u8]) -> Result<Option<usize>, ()> {
    if buf.len() < 4 {
        return Ok(None);
    }
    let len = u16::from_be_bytes([buf[2], buf[3]]) as usize;
    let total = match buf[0] {
        // STUN: top two bits zero, 20-byte header, length excludes it.
        b if b & 0xC0 == 0 => 20 + len,
        // ChannelData: channel numbers 0x4000-0x7FFF, padded on streams.
        0x40..=0x7F => 4 + ((len + 3) & !3),
        _ => return Err(()),
    };
    Ok((buf.len() >= total).then_some(total))
}

fn tls_config() -> Arc<rustls::ClientConfig> {
    static CFG: std::sync::OnceLock<Arc<rustls::ClientConfig>> = std::sync::OnceLock::new();
    CFG.get_or_init(|| {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("tls versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
        Arc::new(cfg)
    })
    .clone()
}

/// Start a bridge to a `turns:` URL. The TLS connection opens in the
/// background; until it is up, webrtc-rs's requests wait in the socket (it
/// retransmits them anyway).
pub(crate) async fn start(turns_url: &str) -> Option<Bridge> {
    let (host, port) = parse_turns(turns_url)?;
    let udp = UdpSocket::bind("127.0.0.1:0").await.ok()?;
    let local = udp.local_addr().ok()?;
    let task = tokio::spawn(run(udp, host, port));
    Some(Bridge {
        url: format!("turn:127.0.0.1:{}?transport=udp", local.port()),
        task,
    })
}

async fn run(udp: UdpSocket, host: String, port: u16) {
    let Ok(name) = ServerName::try_from(host.clone()) else {
        return;
    };
    let connect = async {
        let tcp = TcpStream::connect((host.as_str(), port)).await?;
        let _ = tcp.set_nodelay(true);
        tokio_rustls::TlsConnector::from(tls_config())
            .connect(name, tcp)
            .await
    };
    let tls = match tokio::time::timeout(Duration::from_secs(8), connect).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            log::warn!("partii: TURN over TLS to {host}:{port}: {e}");
            return;
        }
        Err(_) => {
            log::warn!("partii: TURN over TLS to {host}:{port}: timed out");
            return;
        }
    };
    let (mut rd, mut wr) = tokio::io::split(tls);
    let mut peer: Option<SocketAddr> = None;
    let mut dgram = vec![0u8; 64 * 1024];
    let mut acc: Vec<u8> = Vec::with_capacity(16 * 1024);
    let mut chunk = vec![0u8; 16 * 1024];
    loop {
        tokio::select! {
            r = udp.recv_from(&mut dgram) => {
                let Ok((n, from)) = r else { return };
                peer = Some(from);
                if wr.write_all(&dgram[..n]).await.is_err() {
                    return;
                }
                let _ = wr.flush().await;
            }
            r = rd.read(&mut chunk) => {
                let n = match r {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                acc.extend_from_slice(&chunk[..n]);
                loop {
                    match frame_len(&acc) {
                        Ok(Some(total)) => {
                            // Hand webrtc-rs the message without stream padding.
                            let msg_len = if acc[0] & 0xC0 == 0 {
                                total
                            } else {
                                4 + u16::from_be_bytes([acc[2], acc[3]]) as usize
                            };
                            if let Some(p) = peer {
                                let _ = udp.send_to(&acc[..msg_len], p).await;
                            }
                            acc.drain(..total);
                        }
                        Ok(None) => break,
                        Err(()) => {
                            log::warn!("partii: TURN over TLS: bad frame from {host}");
                            return;
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_turns_urls() {
        assert_eq!(
            parse_turns("turns:turn.cloudflare.com:443?transport=tcp"),
            Some(("turn.cloudflare.com".into(), 443))
        );
        assert_eq!(
            parse_turns("turns:t.example"),
            Some(("t.example".into(), 5349))
        );
        assert_eq!(parse_turns("turn:t.example:3478"), None);
    }

    #[test]
    fn frames_stun_and_padded_channel_data() {
        // A 20-byte STUN header with an 8-byte body.
        let mut stun = vec![0x01, 0x01, 0x00, 0x08];
        stun.extend_from_slice(&[0x21, 0x12, 0xA4, 0x42]);
        stun.extend_from_slice(&[0u8; 12 + 8]);
        assert_eq!(frame_len(&stun), Ok(Some(28)));
        assert_eq!(frame_len(&stun[..27]), Ok(None));
        // ChannelData 0x4001 with 5 bytes of data, padded to 8 on a stream.
        let cd = [0x40, 0x01, 0x00, 0x05, 1, 2, 3, 4, 5, 0, 0, 0];
        assert_eq!(frame_len(&cd), Ok(Some(12)));
        assert_eq!(frame_len(&cd[..9]), Ok(None));
        // Back to back: the first message's length, then the next.
        let mut both = cd.to_vec();
        both.extend_from_slice(&stun);
        assert_eq!(frame_len(&both), Ok(Some(12)));
        assert_eq!(frame_len(&both[12..]), Ok(Some(28)));
        assert_eq!(frame_len(&[0x80, 0, 0, 0]), Err(()));
        assert_eq!(frame_len(&[0x40, 0]), Ok(None));
    }

    /// End to end over a loopback TCP "server" without TLS would need the
    /// plain-TCP path; the bridge's framing is what differs from UDP, and
    /// the relay-world run (scripts/relay-world, case b) proves it on
    /// Cloudflare with UDP blocked.
    #[test]
    fn bridge_url_is_plain_udp_turn_on_loopback() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let b = start("turns:127.0.0.1:9?transport=tcp").await.unwrap();
            assert!(b.url.starts_with("turn:127.0.0.1:"));
            assert!(b.url.ends_with("?transport=udp"));
        });
    }
}
