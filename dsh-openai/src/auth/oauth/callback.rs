//! Bounded loopback HTTP reception; unrelated requests cannot consume a login.
use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const MAX_HEADERS: usize = 8192;

impl LoginAttempt {
    pub(super) async fn receive_callback(&self) -> Result<(String, String)> {
        self.receive_callback_with(Duration::from_secs(5)).await
    }

    async fn receive_callback_with(&self, read_timeout: Duration) -> Result<(String, String)> {
        loop {
            let (mut connection, _) = self.listener.accept().await?;
            let target = tokio::time::timeout(read_timeout, read_target(&mut connection)).await;
            // A wrong path/state or incomplete HTTP request is not a response
            // to this authorization attempt. Reject it and keep listening.
            let callback = match target {
                Ok(Ok(target)) if self.matches_callback_state(&target) => {
                    Some(self.callback(&target))
                }
                _ => None,
            };
            let valid = callback.as_ref().is_some_and(|result| result.is_ok());
            let (status, body) = if valid {
                ("200 OK", "Return to doge-shell to complete sign-in.")
            } else {
                (
                    "400 Bad Request",
                    "Sign-in request rejected. Return to doge-shell.",
                )
            };
            // Closing the browser tab must not discard an already-validated
            // callback. Response delivery is best effort and bounded too.
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = tokio::time::timeout(
                Duration::from_secs(1),
                connection.write_all(response.as_bytes()),
            )
            .await;
            if let Some(callback) = callback {
                // A matching-state denial or invalid registration is terminal;
                // never proceed to token exchange or activate an account.
                return callback;
            }
        }
    }

    fn matches_callback_state(&self, target: &str) -> bool {
        let Ok(url) = Url::parse(&format!("http://127.0.0.1{target}")) else {
            return false;
        };
        if url.path() != "/auth/callback" {
            return false;
        }
        let states: Vec<_> = url
            .query_pairs()
            .filter(|(key, _)| key == "state")
            .map(|(_, value)| value.into_owned())
            .collect();
        states.len() == 1 && states[0] == self.state
    }
}

async fn read_target(connection: &mut TcpStream) -> Result<String> {
    let mut request = Vec::new();
    let mut chunk = [0; 512];
    while !request.windows(4).any(|part| part == b"\r\n\r\n") {
        if request.len() >= MAX_HEADERS {
            bail!("OAuth callback headers too large.");
        }
        let remaining = (MAX_HEADERS - request.len()).min(chunk.len());
        let length = connection.read(&mut chunk[..remaining]).await?;
        if length == 0 {
            bail!("Incomplete OAuth callback HTTP.");
        }
        request.extend_from_slice(&chunk[..length]);
    }
    let text =
        std::str::from_utf8(&request).map_err(|_| anyhow!("Invalid OAuth callback HTTP."))?;
    let mut words = text.lines().next().unwrap_or_default().split_whitespace();
    let (method, target, version) = (words.next(), words.next(), words.next());
    if method != Some("GET")
        || !matches!(version, Some("HTTP/1.1" | "HTTP/1.0"))
        || words.next().is_some()
    {
        bail!("Invalid OAuth callback request line.");
    }
    Ok(target.unwrap_or_default().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn attempt() -> (tempfile::TempDir, LoginAttempt) {
        let dir = tempfile::tempdir().unwrap();
        let store = AuthStore::new(std::fs::canonicalize(dir.path()).unwrap().join("auth"));
        let attempt = LoginAttempt::start(store, true, None).await.unwrap();
        (dir, attempt)
    }

    async fn request(address: std::net::SocketAddr, data: &[u8]) -> String {
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream.write_all(data).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    }

    #[tokio::test]
    async fn unrelated_requests_then_fragmented_valid_callback() {
        let (_dir, attempt) = attempt().await;
        let address = attempt.listener.local_addr().unwrap();
        let target = format!(
            "GET /auth/callback?state={}&code=mock-code&client_id=issued HTTP/1.1\r\nHost: localhost\r\n\r\n",
            attempt.state
        );
        let client = async {
            let duplicate = format!(
                "GET /auth/callback?state={}&state={}&code=mock HTTP/1.1\r\n\r\n",
                attempt.state, attempt.state
            );
            for data in [
                "GET /favicon.ico HTTP/1.1\r\n\r\n",
                "GET /auth/callback?state=wrong&error=access_denied HTTP/1.1\r\n\r\n",
                "POST /auth/callback HTTP/1.1\r\n\r\n",
                duplicate.as_str(),
            ] {
                assert!(
                    request(address, data.as_bytes())
                        .await
                        .starts_with("HTTP/1.1 400")
                );
            }
            let mut stream = TcpStream::connect(address).await.unwrap();
            for part in target.as_bytes().chunks(7) {
                stream.write_all(part).await.unwrap();
                tokio::task::yield_now().await;
            }
            let mut response = String::new();
            stream.read_to_string(&mut response).await.unwrap();
            assert!(response.starts_with("HTTP/1.1 200"));
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(attempt.receive_callback(), client)
        })
        .await
        .unwrap();
        assert_eq!(result.unwrap(), ("mock-code".into(), "issued".into()));
        assert!(attempt.store.registrations().unwrap().active.is_none());
    }

    #[tokio::test]
    async fn incomplete_and_oversized_requests_do_not_end_attempt() {
        let (_dir, attempt) = attempt().await;
        let address = attempt.listener.local_addr().unwrap();
        let target = format!(
            "GET /auth/callback?state={}&error=access_denied HTTP/1.1\r\n\r\n",
            attempt.state
        );
        let client = async {
            let _silent = TcpStream::connect(address).await.unwrap();
            let mut oversized = TcpStream::connect(address).await.unwrap();
            oversized.write_all(&vec![b'x'; MAX_HEADERS]).await.unwrap();
            let mut response = String::new();
            oversized.read_to_string(&mut response).await.unwrap();
            assert!(response.starts_with("HTTP/1.1 400"));
            assert!(
                request(address, target.as_bytes())
                    .await
                    .starts_with("HTTP/1.1 400")
            );
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(
                attempt.receive_callback_with(Duration::from_millis(20)),
                client
            )
        })
        .await
        .unwrap();
        assert!(result.unwrap_err().to_string().contains("declined"));
        assert!(attempt.store.registrations().unwrap().active.is_none());
    }

    #[tokio::test]
    async fn cancellation_during_silent_callback_closes_listener() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (_dir, attempt) = attempt().await;
        let address = attempt.listener.local_addr().unwrap();
        let store = attempt.store.clone();
        let _silent = TcpStream::connect(address).await.unwrap();
        let cancelled = AtomicBool::new(false);
        let cancel = || cancelled.load(Ordering::SeqCst);
        let client = async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            cancelled.store(true, Ordering::SeqCst);
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(attempt.finish(Some(&cancel)), client)
        })
        .await
        .unwrap();
        assert!(crate::is_ctrl_c_cancelled(&result.unwrap_err()));
        assert!(TcpStream::connect(address).await.is_err());
        assert!(store.registrations().unwrap().active.is_none());
    }
}
