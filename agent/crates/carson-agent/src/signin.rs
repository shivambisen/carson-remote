//! Browser sign-in for pairing (no code to type), as native apps do it (RFC 8252):
//!
//! 1. The agent listens on `127.0.0.1:<random port>` and opens
//!    `<server>/#/connect?pairing=…&port=…&state=…` in the default browser.
//! 2. The user signs in there (e.g. with Google); the page asks the server for a one-time
//!    grant and navigates to `http://127.0.0.1:<port>/done?state=…&grant=…`.
//! 3. The agent checks `state`, then redeems the grant with the server.
//!
//! The grant only ever reaches the computer whose browser was used, so a pairing link sent by
//! someone else cannot add their computer to your account.

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub struct Loopback {
    listener: TcpListener,
    pub port: u16,
    pub state: String,
}

impl Loopback {
    pub async fn bind() -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .context("listen on 127.0.0.1 for the browser sign-in")?;
        let port = listener.local_addr()?.port();
        let mut raw = [0u8; 16];
        getrandom::fill(&mut raw).map_err(|e| anyhow::anyhow!("OS random generator: {e}"))?;
        let state = raw.iter().map(|b| format!("{b:02x}")).collect();
        Ok(Self {
            listener,
            port,
            state,
        })
    }

    /// The page that starts the sign-in.
    pub fn url(&self, server: &str, pairing_id: &str) -> String {
        format!(
            "{server}/#/connect?pairing={pairing_id}&port={}&state={}",
            self.port, self.state
        )
    }

    /// Waits for the browser to deliver the grant; answers it with a redirect to `done_url`.
    /// Requests with a wrong `state` (or anything else) get a 404 and are ignored.
    pub async fn grant(&self, done_url: &str) -> Result<String> {
        loop {
            let (mut sock, _) = self.listener.accept().await?;
            let mut buf = vec![0u8; 8192];
            let n =
                match tokio::time::timeout(std::time::Duration::from_secs(5), sock.read(&mut buf))
                    .await
                {
                    Ok(Ok(n)) => n,
                    _ => continue,
                };
            let head = String::from_utf8_lossy(&buf[..n]);
            match parse_done(&head, &self.state) {
                Some(grant) => {
                    let resp = format!(
                        "HTTP/1.1 302 Found\r\nLocation: {done_url}\r\nCache-Control: no-store\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    return Ok(grant);
                }
                None => {
                    let _ = sock
                        .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                        .await;
                }
            }
        }
    }
}

/// Extracts the grant from `GET /done?state=…&grant=… HTTP/1.1` if `state` matches.
fn parse_done(head: &str, want_state: &str) -> Option<String> {
    let target = head
        .lines()
        .next()?
        .strip_prefix("GET ")?
        .split(' ')
        .next()?;
    let url = url::Url::parse(&format!("http://127.0.0.1{target}")).ok()?;
    if url.path() != "/done" {
        return None;
    }
    let (mut state, mut grant) = (None, None);
    for (k, v) in url.query_pairs() {
        match k.as_ref() {
            "state" => state = Some(v.into_owned()),
            "grant" => grant = Some(v.into_owned()),
            _ => {}
        }
    }
    // Not secret-dependent timing in any useful way: state is single-use and random.
    (state.as_deref() == Some(want_state))
        .then_some(grant)
        .flatten()
        .filter(|g| !g.is_empty() && g.len() < 200)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_matching_done_requests() {
        let ok = "GET /done?state=abc&grant=G-1_x HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n";
        assert_eq!(parse_done(ok, "abc").as_deref(), Some("G-1_x"));
        assert_eq!(parse_done(ok, "abd"), None, "wrong state");
        assert_eq!(parse_done("GET /done?grant=G HTTP/1.1\r\n", "abc"), None);
        assert_eq!(
            parse_done("GET /other?state=abc&grant=G HTTP/1.1\r\n", "abc"),
            None
        );
        assert_eq!(
            parse_done("POST /done?state=abc&grant=G HTTP/1.1\r\n", "abc"),
            None
        );
        assert_eq!(
            parse_done("GET /done?state=abc&grant= HTTP/1.1\r\n", "abc"),
            None
        );
    }

    #[tokio::test]
    async fn delivers_grant_and_redirects() {
        let lb = Loopback::bind().await.unwrap();
        let (port, state) = (lb.port, lb.state.clone());
        let browser = tokio::spawn(async move {
            // A stray request first (e.g. favicon), then the real one.
            for path in [
                "/favicon.ico".to_owned(),
                format!("/done?state={state}&grant=GR"),
            ] {
                let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port))
                    .await
                    .unwrap();
                s.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
                    .await
                    .unwrap();
                let mut out = String::new();
                s.read_to_string(&mut out).await.unwrap();
                if path.starts_with("/done") {
                    return out;
                }
            }
            unreachable!()
        });
        assert_eq!(
            lb.grant("https://c.example/#/connected").await.unwrap(),
            "GR"
        );
        let resp = browser.await.unwrap();
        assert!(
            resp.starts_with("HTTP/1.1 302")
                && resp.contains("Location: https://c.example/#/connected")
        );
    }
}
