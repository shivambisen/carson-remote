//! TURN over TLS/TCP for webrtc-rs, whose relay client only speaks UDP: it skips `turns:` and
//! `?transport=tcp` URLs outright. Those are exactly the ones that get through campus and office
//! firewalls (TLS on port 443 looks like any HTTPS connection), so without this a host on such a
//! network can only reach the relay over plain UDP — which the same firewall blocks.
//!
//! For each such URL, a session gets a small UDP listener on this machine's own address, handed
//! to webrtc-rs as an ordinary `turn:…?transport=udp` server with the same credentials. Every
//! datagram it receives is forwarded to the real relay over one TLS (or TCP) connection per
//! local client, framed as RFC 8656 requires for stream transports, and replies come back the
//! same way. The relay sees a normal TURN-over-TLS client; nothing about TURN itself changes.
//!
//! The listener is on the LAN address rather than loopback because webrtc-rs never sends from a
//! loopback socket; it only accepts datagrams from that same address (this process's own
//! sockets), so other machines on the network cannot use it.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use webrtc::peer_connection::RTCIceServer;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// After a failed connection, drop datagrams for this long instead of reconnecting on each one.
const RECONNECT_BACKOFF: Duration = Duration::from_secs(5);
/// webrtc-rs opens one TURN client per local interface; a handful is all that ever appears.
const MAX_CLIENTS: usize = 8;

/// Keeps a session's bridges running; dropping it stops them.
#[derive(Default)]
pub struct Bridges {
    _tasks: JoinSet<()>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Target {
    tls: bool,
    host: String,
    port: u16,
}

/// Parses a TURN URL that webrtc-rs cannot use itself (TLS, or TCP) into where to connect.
fn stream_target(url: &str) -> Option<Target> {
    let (tls, rest) = if let Some(r) = url.strip_prefix("turns:") {
        (true, r)
    } else {
        (false, url.strip_prefix("turn:")?)
    };
    let (hostport, query) = rest.split_once('?').unwrap_or((rest, ""));
    let transport = query
        .split('&')
        .find_map(|kv| kv.strip_prefix("transport="))
        .unwrap_or(if tls { "tcp" } else { "udp" });
    if transport != "tcp" {
        return None; // plain UDP: webrtc-rs handles it; "turns" over UDP (DTLS) is not offered
    }
    let default_port = if tls { 5349 } else { 3478 };
    let (host, port) = match hostport.strip_prefix('[') {
        Some(v6) => {
            let (h, after) = v6.split_once(']')?;
            let port = after
                .strip_prefix(':')
                .map_or(Some(default_port), |p| p.parse().ok())?;
            (h.to_owned(), port)
        }
        None => match hostport.rsplit_once(':') {
            Some((h, p)) => (h.to_owned(), p.parse().ok()?),
            None => (hostport.to_owned(), default_port),
        },
    };
    if host.is_empty() {
        return None;
    }
    Some(Target { tls, host, port })
}

/// The address this machine uses to reach the internet (no packet is sent).
fn primary_ipv4() -> Option<IpAddr> {
    let probe = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    probe.connect("192.0.2.1:9").ok()?; // TEST-NET-1: never routed anywhere, just picks a route
    let ip = probe.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
}

/// Replaces every TLS/TCP TURN URL in `servers` with a local UDP bridge to it. Returns the
/// servers to give webrtc-rs and the bridges, which must outlive the peer connection.
pub async fn expand(servers: Vec<RTCIceServer>) -> (Vec<RTCIceServer>, Bridges) {
    let mut bridges = Bridges::default();
    let wants_bridge = servers
        .iter()
        .any(|s| s.urls.iter().any(|u| stream_target(u).is_some()));
    if !wants_bridge {
        return (servers, bridges);
    }
    let Some(local_ip) = primary_ipv4() else {
        tracing::warn!("no usable local address; TLS/TCP relays unavailable");
        return (servers, bridges);
    };

    let mut out = Vec::with_capacity(servers.len());
    let mut seen = HashMap::new();
    for server in servers {
        let (stream_urls, udp_urls): (Vec<_>, Vec<_>) = server
            .urls
            .iter()
            .cloned()
            .partition(|u| stream_target(u).is_some());
        for url in stream_urls {
            let target = stream_target(&url).expect("partitioned on this");
            let key = (target.clone(), server.username.clone());
            let local = if let Some(addr) = seen.get(&key) {
                *addr
            } else {
                let socket = match UdpSocket::bind(SocketAddr::new(local_ip, 0)).await {
                    Ok(s) => Arc::new(s),
                    Err(e) => {
                        tracing::warn!(error = %e, "could not open a relay bridge");
                        continue;
                    }
                };
                let addr = match socket.local_addr() {
                    Ok(a) => a,
                    Err(_) => continue,
                };
                tracing::info!(relay = %url, bridge = %addr, "bridging TLS/TCP relay for webrtc");
                bridges._tasks.spawn(serve(socket, target));
                seen.insert(key, addr);
                addr
            };
            out.push(RTCIceServer {
                urls: vec![format!("turn:{local}?transport=udp")],
                username: server.username.clone(),
                credential: server.credential.clone(),
            });
        }
        if !udp_urls.is_empty() {
            out.push(RTCIceServer {
                urls: udp_urls,
                username: server.username,
                credential: server.credential,
            });
        }
    }
    (out, bridges)
}

struct Client {
    tx: mpsc::Sender<Vec<u8>>,
    started: Instant,
}

/// Receives webrtc-rs's datagrams and hands each local client's to its own stream connection.
async fn serve(socket: Arc<UdpSocket>, target: Target) {
    let own_ip = socket.local_addr().map(|a| a.ip()).ok();
    let mut clients: HashMap<SocketAddr, Client> = HashMap::new();
    let mut tasks = JoinSet::new();
    let mut buf = vec![0u8; 65_536];
    loop {
        let (n, from) = match socket.recv_from(&mut buf).await {
            Ok(v) => v,
            Err(e) => {
                // Windows reports ICMP "port unreachable" as a recv error; never spin on it.
                tracing::debug!(error = %e, "relay bridge recv");
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        if Some(from.ip()) != own_ip {
            continue; // only this machine's own sockets may use the bridge
        }
        let datagram = buf[..n].to_vec();
        if let Some(c) = clients.get(&from)
            && !c.tx.is_closed()
        {
            let _ = c.tx.try_send(datagram); // full: drop, as UDP would
            continue;
        }
        if clients
            .get(&from)
            .is_some_and(|c| c.started.elapsed() < RECONNECT_BACKOFF)
        {
            continue; // its connection just failed; let the TURN client's retries wait it out
        }
        clients.retain(|_, c| !c.tx.is_closed());
        if clients.len() >= MAX_CLIENTS {
            continue;
        }
        let (tx, rx) = mpsc::channel(256);
        let _ = tx.try_send(datagram);
        clients.insert(
            from,
            Client {
                tx,
                started: Instant::now(),
            },
        );
        tasks.spawn(relay_client(Arc::clone(&socket), from, target.clone(), rx));
    }
}

/// One local TURN client's connection to the relay, for as long as it lasts.
async fn relay_client(
    socket: Arc<UdpSocket>,
    client: SocketAddr,
    target: Target,
    rx: mpsc::Receiver<Vec<u8>>,
) {
    let result = async {
        let tcp = tokio::time::timeout(
            CONNECT_TIMEOUT,
            TcpStream::connect((target.host.as_str(), target.port)),
        )
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "connect timed out"))??;
        let _ = tcp.set_nodelay(true);
        if target.tls {
            let name = rustls::pki_types::ServerName::try_from(target.host.clone())
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
            let tls = tokio::time::timeout(
                CONNECT_TIMEOUT,
                tokio_rustls::TlsConnector::from(tls_config()).connect(name, tcp),
            )
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "TLS handshake timed out"))??;
            tracing::info!(relay = %target.host, port = target.port, "relay bridge connected over TLS");
            pump(tls, &socket, client, rx).await
        } else {
            tracing::info!(relay = %target.host, port = target.port, "relay bridge connected over TCP");
            pump(tcp, &socket, client, rx).await
        }
    }
    .await;
    if let Err(e) = result {
        tracing::warn!(relay = %target.host, port = target.port, error = %e, "relay bridge closed");
    }
}

async fn pump<S: AsyncRead + AsyncWrite>(
    stream: S,
    socket: &UdpSocket,
    client: SocketAddr,
    mut rx: mpsc::Receiver<Vec<u8>>,
) -> io::Result<()> {
    let (mut rd, mut wr) = tokio::io::split(stream);
    let up = async {
        while let Some(datagram) = rx.recv().await {
            wr.write_all(&to_stream(&datagram)).await?;
        }
        io::Result::Ok(())
    };
    let down = async {
        loop {
            let frame = read_frame(&mut rd).await?;
            socket.send_to(&frame, client).await?;
        }
    };
    tokio::select! {
        r = up => r,
        r = down => r,
    }
}

/// A datagram as sent on a stream: ChannelData is padded to a multiple of 4 bytes (STUN
/// messages always are).
fn to_stream(datagram: &[u8]) -> Vec<u8> {
    let mut out = datagram.to_vec();
    if datagram.first().is_some_and(|b| b & 0xC0 == 0x40) {
        out.resize(datagram.len().div_ceil(4) * 4, 0);
    }
    out
}

/// Reads one STUN message or ChannelData message off a stream, returning it as a datagram.
async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<Vec<u8>> {
    let mut header = [0u8; 4];
    r.read_exact(&mut header).await?;
    let len = usize::from(u16::from_be_bytes([header[2], header[3]]));
    let (body, keep) = match header[0] & 0xC0 {
        0x00 => (16 + len, 16 + len), // STUN: 20-byte header, length counts attributes
        0x40 => (len.div_ceil(4) * 4, len), // ChannelData: padded on streams
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a TURN message",
            ));
        }
    };
    let mut frame = vec![0u8; 4 + body];
    frame[..4].copy_from_slice(&header);
    r.read_exact(&mut frame[4..]).await?;
    frame.truncate(4 + keep);
    Ok(frame)
}

fn tls_config() -> Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    Arc::clone(CONFIG.get_or_init(|| {
        let mut roots = rustls::RootCertStore::empty();
        let native = rustls_native_certs::load_native_certs();
        for cert in native.certs {
            let _ = roots.add(cert);
        }
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .expect("aws-lc-rs supports the default TLS versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
        Arc::new(config)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_out_only_the_urls_webrtc_rs_cannot_use() {
        let t = |tls, host: &str, port| {
            Some(Target {
                tls,
                host: host.into(),
                port,
            })
        };
        assert_eq!(
            stream_target("turns:global.relay.metered.ca:443?transport=tcp"),
            t(true, "global.relay.metered.ca", 443)
        );
        assert_eq!(
            stream_target("turn:global.relay.metered.ca:80?transport=tcp"),
            t(false, "global.relay.metered.ca", 80)
        );
        assert_eq!(
            stream_target("turns:turn.example"),
            t(true, "turn.example", 5349)
        );
        assert_eq!(
            stream_target("turns:[2001:db8::1]:443"),
            t(true, "2001:db8::1", 443)
        );
        assert_eq!(stream_target("turn:global.relay.metered.ca:80"), None);
        assert_eq!(stream_target("turn:x:3478?transport=udp"), None);
        assert_eq!(stream_target("stun:stun.l.google.com:19302"), None);
        assert_eq!(stream_target("turns:"), None);
    }

    #[tokio::test]
    async fn stream_framing_round_trips_stun_and_padded_channel_data() {
        // A STUN binding request (20-byte header, 4 bytes of attributes) and a 5-byte ChannelData.
        let mut stun = vec![0x00, 0x01, 0x00, 0x04];
        stun.extend_from_slice(&[0x21, 0x12, 0xA4, 0x42]);
        stun.extend_from_slice(&[7u8; 12]);
        stun.extend_from_slice(&[0, 0, 0, 0]);
        let chan = vec![0x40, 0x00, 0x00, 0x05, 1, 2, 3, 4, 5];

        let padded = to_stream(&chan);
        assert_eq!(
            padded.len(),
            12,
            "ChannelData is padded to 4 bytes on streams"
        );
        assert_eq!(to_stream(&stun), stun, "STUN is sent as is");

        let mut wire = to_stream(&stun);
        wire.extend_from_slice(&padded);
        let mut r = std::io::Cursor::new(wire);
        assert_eq!(read_frame(&mut r).await.unwrap(), stun);
        assert_eq!(
            read_frame(&mut r).await.unwrap(),
            chan,
            "padding is stripped"
        );
        assert!(
            read_frame(&mut r).await.is_err(),
            "clean EOF ends the stream"
        );
    }

    #[tokio::test]
    async fn rejects_garbage_instead_of_misframing() {
        let mut r = std::io::Cursor::new(vec![0x80, 0x00, 0x00, 0x00]);
        assert!(read_frame(&mut r).await.is_err());
    }

    #[tokio::test]
    async fn leaves_udp_only_servers_untouched() {
        let servers = vec![RTCIceServer {
            urls: vec![
                "stun:stun.l.google.com:19302".into(),
                "turn:t.example:3478".into(),
            ],
            username: "u".into(),
            credential: "c".into(),
        }];
        let (out, _b) = expand(servers.clone()).await;
        assert_eq!(out, servers);
    }

    #[tokio::test]
    async fn swaps_tls_urls_for_a_local_udp_bridge_with_the_same_credentials() {
        let servers = vec![RTCIceServer {
            urls: vec![
                "turn:t.example:80".into(),
                "turns:t.example:443?transport=tcp".into(),
            ],
            username: "u".into(),
            credential: "c".into(),
        }];
        let (out, _b) = expand(servers).await;
        if primary_ipv4().is_none() {
            return; // offline test machine: nothing to bridge from
        }
        assert_eq!(out.len(), 2);
        assert!(out[0].urls[0].starts_with("turn:") && out[0].urls[0].ends_with("?transport=udp"));
        assert!(!out[0].urls[0].contains("t.example"));
        assert_eq!(
            (out[0].username.as_str(), out[0].credential.as_str()),
            ("u", "c")
        );
        assert_eq!(out[1].urls, vec!["turn:t.example:80".to_string()]);
    }
}
