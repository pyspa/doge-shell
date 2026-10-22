//! Exercise the production provider and client builders with an inert local CA.
use super::*;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::sync::Mutex;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_rustls::{TlsAcceptor, rustls};

const CA: &[u8] = include_bytes!("redirect-fixtures/ca.pem");
const KEY: &str = "inert-redirect-fixture-key";

struct Server {
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
    shutdown: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl Server {
    async fn start(tls: bool, status: u16, location: Option<&str>, body: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let url = format!("{}://{address}", if tls { "https" } else { "http" });
        let acceptor = tls.then(|| {
            let cert =
                CertificateDer::from_pem_slice(include_bytes!("redirect-fixtures/server.pem"))
                    .unwrap();
            let key = PrivateKeyDer::from_pem_slice(include_bytes!("redirect-fixtures/server.key"))
                .unwrap();
            let config = rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(vec![cert], key)
                .unwrap();
            TlsAcceptor::from(Arc::new(config))
        });
        let response = format!(
            "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n{body}",
            body.len(),
            location
                .map(|url| format!("Location: {url}\r\n"))
                .unwrap_or_default()
        );
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let (shutdown, mut stop) = oneshot::channel();
        let task = tokio::spawn(async move {
            let deadline = tokio::time::sleep(Duration::from_secs(20));
            tokio::pin!(deadline);
            loop {
                tokio::select! {
                    _ = &mut stop => break,
                    _ = &mut deadline => break,
                    accepted = listener.accept() => {
                        let (stream, _) = accepted.unwrap();
                        // Bound handshake, headers, body, and response as well as accept.
                        let request = async {
                            if let Some(acceptor) = &acceptor {
                                if let Ok(stream) = acceptor.accept(stream).await {
                                    exchange(stream, &response, &captured).await;
                                }
                            } else {
                                exchange(stream, &response, &captured).await;
                            }
                        };
                        let _ = tokio::time::timeout(Duration::from_secs(2), request).await;
                    }
                }
            }
        });
        Self {
            url,
            requests,
            shutdown: Some(shutdown),
            task,
        }
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    async fn stop(&mut self) {
        let _ = self.shutdown.take().unwrap().send(());
        tokio::time::timeout(Duration::from_secs(3), &mut self.task)
            .await
            .unwrap()
            .unwrap();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Also owns cleanup when an assertion fails.
        self.task.abort();
    }
}

async fn exchange<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    response: &str,
    captured: &Mutex<Vec<String>>,
) {
    let mut bytes = Vec::new();
    let mut buffer = [0; 2048];
    loop {
        let n = stream.read(&mut buffer).await.unwrap();
        if n == 0 {
            return;
        }
        bytes.extend_from_slice(&buffer[..n]);
        assert!(bytes.len() < 64 * 1024, "fixture request too large");
        if let Some(end) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
            let length: usize = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .unwrap()
                .parse()
                .unwrap();
            if bytes.len() >= end + 4 + length {
                break;
            }
        }
    }
    captured
        .lock()
        .unwrap()
        .push(String::from_utf8(bytes).unwrap());
    stream.write_all(response.as_bytes()).await.unwrap();
    stream.shutdown().await.unwrap();
}

fn client(protocol: &str, url: &str, allow: bool, trust_ca: bool) -> ChatGptClient {
    let config = OpenAiConfig::from_getter(|key| match key {
        "AI_CHAT_API_KEY" => Some(KEY.into()),
        "AI_CHAT_API" => Some(protocol.into()),
        "AI_CHAT_BASE_URL" => Some(url.into()),
        "AI_CHAT_MODEL" => Some("inert-fixture-model".into()),
        "AI_CHAT_ALLOW_INSECURE_HTTP" => Some(allow.to_string()),
        _ => None,
    })
    .with_timeout(Duration::from_secs(2));
    ChatGptClient::try_from_config_with_builder(&config, &|builder| {
        let builder = builder.no_proxy();
        if trust_ca {
            builder.tls_certs_only([reqwest::Certificate::from_pem(CA).unwrap()])
        } else {
            builder
        }
    })
    .unwrap()
}

fn response(protocol: &str) -> String {
    if protocol == "responses" {
        json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"fixture-ok"}]}]}).to_string()
    } else {
        json!({"choices":[{"message":{"role":"assistant","content":"fixture-ok"},"finish_reason":"stop"}]}).to_string()
    }
}

fn send(client: &ChatGptClient) -> Result<Value> {
    let started = std::time::Instant::now();
    client.send_chat(
        &[
            json!({"role":"user","content":"inert conversation fixture"}),
            json!({"role":"tool","tool_call_id":"fixture-call","content":"inert tool result"}),
        ],
        &ChatRequestOptions::new(),
        Some(&|| started.elapsed() > Duration::from_secs(5)),
    )
}

async fn downgrade(protocol: &str, status: u16) {
    for allow in [false, true] {
        let mut sink = Server::start(false, 200, None, &response(protocol)).await;
        let mut origin = Server::start(true, status, Some(&sink.url), "").await;
        let result = send(&client(protocol, &origin.url, allow, true));
        origin.stop().await;
        sink.stop().await;
        let sent = origin.requests();
        let received = sink.requests();
        assert!(!sent.is_empty(), "must reach authenticated TLS origin");
        assert!(sent.iter().all(|r| {
            r.to_ascii_lowercase()
                .contains(&format!("authorization: bearer {KEY}"))
        }));
        if allow {
            assert_eq!(
                crate::turn::answer_text(&result.unwrap()).unwrap(),
                "fixture-ok"
            );
            assert_eq!(received.len(), 1);
            assert_eq!(
                received[0].split_once("\r\n\r\n").unwrap().1,
                sent[0].split_once("\r\n\r\n").unwrap().1
            );
            assert!(!received[0].to_ascii_lowercase().contains("authorization:"));
            assert!(received[0].contains("inert conversation fixture"));
            assert!(received[0].contains("inert tool result"));
        } else {
            assert_eq!(
                received.len(),
                0,
                "HTTPS downgrade disclosed the request body"
            );
            assert!(result.is_err(), "HTTPS downgrade must fail");
        }
    }
}

macro_rules! downgrade_test {
    ($name:ident, $protocol:literal, $status:literal) => {
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn $name() {
            downgrade($protocol, $status).await;
        }
    };
}
downgrade_test!(chat_https_downgrade_307, "chat_completions", 307);
downgrade_test!(chat_https_downgrade_308, "chat_completions", 308);
downgrade_test!(responses_https_downgrade_307, "responses", 307);
downgrade_test!(responses_https_downgrade_308, "responses", 308);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn secure_redirects_and_explicit_http_remain_supported() {
    for protocol in ["chat_completions", "responses"] {
        for status in [307, 308] {
            let mut sink = Server::start(true, 200, None, &response(protocol)).await;
            let mut origin = Server::start(true, status, Some(&sink.url), "").await;
            let result = send(&client(protocol, &origin.url, false, true)).unwrap();
            assert_eq!(crate::turn::answer_text(&result).unwrap(), "fixture-ok");
            origin.stop().await;
            sink.stop().await;
            assert_eq!(sink.requests().len(), 1);
            assert_eq!(
                sink.requests()[0].split_once("\r\n\r\n").unwrap().1,
                origin.requests()[0].split_once("\r\n\r\n").unwrap().1
            );
        }
        let mut http = Server::start(false, 200, None, &response(protocol)).await;
        let result = send(&client(protocol, &http.url, true, false)).unwrap();
        assert_eq!(crate::turn::answer_text(&result).unwrap(), "fixture-ok");
        http.stop().await;
        assert_eq!(http.requests().len(), 1);
    }
}
