use super::*;
#[test]
fn scopes_are_required_and_debug_is_private() {
    let credentials = Credentials {
        access_token: "secret-a".into(),
        refresh_token: "secret-r".into(),
        id_token: "secret-i".into(),
        scopes: vec![],
        expires_at: 0,
        earliest_refresh_at: 0,
    };
    assert!(credentials.check_scopes().is_err());
    assert!(!format!("{:?}", AuthStore::new("/secret-path".into())).contains("secret-path"));
}
#[test]
fn untrusted_diagnostic_fields_are_not_echoed() {
    assert_eq!(identifier(Some("token=secret")), "unknown");
    assert_eq!(identifier(Some("invalid_grant")), "invalid_grant");
}

pub(crate) fn seeded_store(root: std::path::PathBuf) -> (AuthStore, Account) {
    // macOS temporary paths can traverse the OS /var symlink. Fixtures use its
    // canonical parent so the production symlink rejection stays meaningful.
    let root = std::fs::canonicalize(root.parent().unwrap())
        .unwrap()
        .join(root.file_name().unwrap());
    let store = AuthStore::new(root);
    store.init().unwrap();
    let account = Account {
        label: uuid::Uuid::new_v4().to_string(),
        client_id: "mock-issued".into(),
        subject: "mock-account".into(),
    };
    let reg = store::Registrations {
        host_id: "urn:uuid:mock-host".into(),
        active: Some(account.label.clone()),
        accounts: vec![account.clone()],
    };
    store.save("registration.json", &reg).unwrap();
    store
        .save_credentials(
            &account,
            &Credentials {
                access_token: "mock-access".into(),
                refresh_token: "mock-refresh".into(),
                id_token: "mock-id".into(),
                scopes: vec![
                    "resource.invoke".into(),
                    "chatgpt.tokens.use.direct".into(),
                    "offline_access".into(),
                ],
                expires_at: now() + 3600,
                earliest_refresh_at: 0,
            },
        )
        .unwrap();
    (store, account)
}
#[test]
fn owner_only_atomic_storage_and_symlinks() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let dir = tempfile::tempdir().unwrap();
    let (store, account) = seeded_store(dir.path().join("subscription-auth"));
    assert_eq!(
        std::fs::metadata(&store.root).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let path = store
        .root
        .join(AuthStore::credential_name(&account.label).unwrap());
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let mut creds = store.credentials(&account).unwrap();
    creds.refresh_token = "mock-rotated".into();
    store.save_credentials(&account, &creds).unwrap();
    assert_eq!(
        store.credentials(&account).unwrap().refresh_token,
        "mock-rotated"
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(store.status().is_err());
    std::fs::remove_file(&path).unwrap();
    symlink(dir.path().join("missing"), &path).unwrap();
    assert!(store.status().is_err());
    assert!(store.save_credentials(&account, &creds).is_err());
    let link = dir.path().join("linked");
    symlink(&store.root, &link).unwrap();
    assert!(AuthStore::new(link).status().is_err());
}
#[tokio::test]
async fn refresh_is_serialized_and_rotates_once() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let dir = tempfile::tempdir().unwrap();
    let (mut store, account) = seeded_store(dir.path().join("subscription-auth"));
    let mut creds = store.credentials(&account).unwrap();
    creds.expires_at = now();
    store.save_credentials(&account, &creds).unwrap();
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    store.token_endpoint = Some(format!("http://{}/token", listener.local_addr().unwrap()));
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = vec![0; 4096];
        let n = stream.read(&mut bytes).await.unwrap();
        let request = String::from_utf8_lossy(&bytes[..n]);
        assert!(request.contains("grant_type=refresh_token"));
        assert!(request.contains("client_id=mock-issued"));
        assert!(request.contains("refresh_token=mock-refresh"));
        assert!(!request.contains("scope="));
        assert!(!request.contains("client_secret="));
        let body=serde_json::json!({"access_token":"mock-new-access","refresh_token":"mock-new-refresh","token_type":"Bearer","expires_in":3600,"scope":"resource.invoke chatgpt.tokens.use.direct offline_access","earliest_refresh_at":now()+120}).to_string();
        stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(200), listener.accept())
                .await
                .is_err()
        );
    });
    let first = store.clone();
    let second = store.clone();
    let (a, b) = tokio::join!(first.access(None), second.access(None));
    assert_eq!(a.unwrap().1, "mock-new-access");
    assert_eq!(b.unwrap().1, "mock-new-access");
    task.await.unwrap();
    assert_eq!(
        store.credentials(&account).unwrap().refresh_token,
        "mock-new-refresh"
    );
    let lock = store.lock(None).await.unwrap();
    assert!(store.lock(Some(&|| true)).await.is_err());
    drop(lock);
}
#[tokio::test]
async fn earliest_refresh_is_respected_without_network() {
    let dir = tempfile::tempdir().unwrap();
    let (store, account) = seeded_store(dir.path().join("subscription-auth"));
    let mut creds = store.credentials(&account).unwrap();
    creds.expires_at = now() - 1;
    creds.earliest_refresh_at = now() + 60;
    store.save_credentials(&account, &creds).unwrap();
    assert!(
        store
            .access(None)
            .await
            .unwrap_err()
            .to_string()
            .contains("before refresh is allowed")
    );
}

pub(crate) async fn mock_http(
    replies: Vec<(u16, &'static str, String)>,
) -> (
    String,
    std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    tokio::task::JoinHandle<()>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let endpoint = format!("http://{}/mock", listener.local_addr().unwrap());
    let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = requests.clone();
    let task = tokio::spawn(async move {
        for (status, content_type, body) in replies {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut chunk = [0; 4096];
                let n = stream.read(&mut chunk).await.unwrap();
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(&chunk[..n]);
                if let Some(header_end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&bytes[..header_end]);
                    let length = header
                        .lines()
                        .find_map(|l| {
                            l.to_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= header_end + 4 + length {
                        break;
                    }
                }
            }
            captured
                .lock()
                .unwrap()
                .push(String::from_utf8(bytes).unwrap());
            stream.write_all(format!("HTTP/1.1 {status} Mock\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
    });
    (endpoint, requests, task)
}
#[tokio::test]
async fn terminal_refresh_revokes_local_tokens_but_preserves_registration() {
    let dir = tempfile::tempdir().unwrap();
    let (mut store, account) = seeded_store(dir.path().join("subscription-auth"));
    let mut credentials = store.credentials(&account).unwrap();
    credentials.expires_at = 0;
    store.save_credentials(&account, &credentials).unwrap();
    let (url, _, task) = mock_http(vec![(
        400,
        "application/json",
        r#"{"error":"invalid_grant","error_description":"mock-secret-do-not-display"}"#.into(),
    )])
    .await;
    store.token_endpoint = Some(url);
    let error = store.access(None).await.unwrap_err().to_string();
    task.await.unwrap();
    assert!(error.contains("reauthenticate"));
    assert!(!error.contains("mock-secret"));
    assert!(store.credentials(&account).is_err());
    assert_eq!(store.accounts().unwrap().len(), 1);
}
