use super::{config::ServeConfig, server::build_router};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use std::{fs, path::Path};
use tempfile::TempDir;
use tower::ServiceExt;

async fn request(
    root: &Path,
    path: &str,
    index: bool,
    cors: bool,
) -> (StatusCode, axum::http::HeaderMap, String) {
    let app = build_router(&ServeConfig {
        directory: root.to_path_buf(),
        serve_index: index,
        enable_cors: cors,
        ..ServeConfig::default()
    });
    let response = app
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, headers, String::from_utf8(body.to_vec()).unwrap())
}

#[tokio::test]
async fn production_serve_router_constructs_and_serves_root_and_nested() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("index.html"), "root index").unwrap();
    fs::create_dir(dir.path().join("nested")).unwrap();
    fs::write(dir.path().join("nested/index.html"), "nested index").unwrap();
    for cors in [false, true] {
        for (path, expected) in [("/", "root index"), ("/nested/", "nested index")] {
            let (status, headers, body) = request(dir.path(), path, true, cors).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body, expected);
            assert_eq!(headers.contains_key("access-control-allow-origin"), cors);
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn production_serve_router_rejects_outside_implicit_indexes() {
    use std::os::unix::fs::symlink;
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("root");
    fs::create_dir(&root).unwrap();
    fs::create_dir(root.join("nested")).unwrap();
    let outside = dir.path().join("outside.html");
    fs::write(&outside, "OUTSIDE_SENTINEL").unwrap();
    for (url, directory, relative) in [
        ("/", root.clone(), "../outside.html"),
        ("/nested/", root.join("nested"), "../../outside.html"),
    ] {
        for target in [outside.as_path(), Path::new(relative)] {
            let index = directory.join("index.html");
            symlink(target, &index).unwrap();
            let (status, _, body) = request(&root, url, true, false).await;
            assert_eq!(status, StatusCode::FORBIDDEN);
            assert!(!body.contains("OUTSIDE_SENTINEL"));
            fs::remove_file(index).unwrap();
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn production_serve_router_preserves_index_and_listing_contracts() {
    use std::os::unix::fs::symlink;
    let dir = TempDir::new().unwrap();
    fs::create_dir(dir.path().join("nested")).unwrap();
    fs::write(dir.path().join("a b.txt"), "safe file").unwrap();
    fs::write(dir.path().join("<tag>.txt"), "escaped name").unwrap();
    for (url, directory, target) in [
        ("/", dir.path().to_path_buf(), "a b.txt"),
        ("/nested/", dir.path().join("nested"), "../a b.txt"),
    ] {
        let index = directory.join("index.html");
        symlink(target, &index).unwrap();
        let (status, _, body) = request(dir.path(), url, true, false).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "safe file");
        let (status, _, body) = request(dir.path(), url, false, true).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Directory Listing"));
        fs::remove_file(&index).unwrap();
        symlink("missing-target", &index).unwrap();
        assert_eq!(
            request(dir.path(), url, true, false).await.0,
            StatusCode::NOT_FOUND
        );
        fs::remove_file(&index).unwrap();
        fs::create_dir(&index).unwrap();
        let (status, headers, body) = request(dir.path(), url, true, true).await;
        assert_eq!(status, StatusCode::OK);
        assert!(headers.contains_key("access-control-allow-origin"));
        assert!(body.contains("Directory Listing"));
        fs::remove_dir(&index).unwrap();
        let (status, _, body) = request(dir.path(), url, true, false).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Directory Listing"));
    }
    let (_, _, body) = request(dir.path(), "/", false, false).await;
    assert!(body.contains("&lt;tag&gt;.txt"));
    assert!(!body.contains("<tag>.txt"));
    assert_eq!(
        request(dir.path(), "/a%20b.txt", true, false).await.2,
        "safe file"
    );
    symlink(dir.path().join("nested"), dir.path().join("alias")).unwrap();
    assert_eq!(
        request(dir.path(), "/alias/", true, false).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn production_serve_router_blocks_encoded_traversal() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("root");
    fs::create_dir(&root).unwrap();
    fs::write(dir.path().join("outside.txt"), "OUTSIDE_SENTINEL").unwrap();
    for url in ["/%2e%2e/outside.txt", "/%2e%2e%2foutside.txt"] {
        let (status, _, body) = request(&root, url, true, false).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(!body.contains("OUTSIDE_SENTINEL"));
    }
}

#[test]
fn serve_occupied_port_returns_error_and_nonzero_builtin_status() {
    use super::{error::ServeError, server::start_http_server};
    use std::{
        net::TcpListener,
        process::{Command, Stdio},
        time::Duration,
    };
    use wait_timeout::ChildExt;

    const CHILD_PORT: &str = "DOGESH_SERVE_OCCUPIED_TEST_PORT";
    const CHILD_ROOT: &str = "DOGESH_SERVE_OCCUPIED_TEST_ROOT";
    if let Ok(port) = std::env::var(CHILD_PORT) {
        let port = port.parse().unwrap();
        let config = ServeConfig {
            directory: std::env::var_os(CHILD_ROOT).unwrap().into(),
            port,
            ..ServeConfig::default()
        };
        let pid = nix::unistd::getpid();
        let ctx = dsh_types::Context::new_safe(pid, pid, false);
        let result = start_http_server(&ctx, config.clone());
        let status = super::command(
            &ctx,
            vec![
                "serve".into(),
                "-p".into(),
                port.to_string(),
                config.directory.to_string_lossy().into_owned(),
            ],
            &mut crate::test_support::TestShellProxy::default(),
        );
        assert!(matches!(result, Err(ServeError::PortInUse { port: actual }) if actual == port));
        assert_eq!(status, dsh_types::ExitStatus::ExitedWith(1));
        return;
    }

    let dir = TempDir::new().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    // A child bounds the synchronous builtin path as well as its runtime threads.
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "serve::router_tests::serve_occupied_port_returns_error_and_nonzero_builtin_status",
            "--nocapture",
        ])
        .env(CHILD_PORT, port.to_string())
        .env(CHILD_ROOT, dir.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let waited = child.wait_timeout(Duration::from_secs(10));
    if !matches!(waited, Ok(Some(_))) {
        let _ = child.kill();
        child
            .wait()
            .expect("reap the owned test child after timeout/error");
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        matches!(waited, Ok(Some(_))),
        "occupied port must fail promptly: {waited:?}"
    );
    assert!(
        output.status.success(),
        "child failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn production_serve_router_loopback_smoke() {
    use std::time::Duration;
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("index.html"), "loopback root").unwrap();
    fs::create_dir(dir.path().join("nested")).unwrap();
    fs::write(dir.path().join("nested/a b.txt"), "loopback nested").unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = build_router(&ServeConfig {
        directory: dir.path().to_path_buf(),
        ..ServeConfig::default()
    });
    let task = tokio::spawn(async move { axum::serve(listener, app).await });
    // Collect results before assertions so every request outcome cleans up the listener.
    let results = async {
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()?;
        let root = client
            .get(format!("http://{address}/"))
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        let nested = client
            .get(format!("http://{address}/nested/a%20b.txt"))
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        Ok::<_, reqwest::Error>((root, nested))
    }
    .await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let (root, nested) = results.unwrap();
    assert_eq!(root, "loopback root");
    assert_eq!(nested, "loopback nested");
    // A fresh bind proves the owned listener was released by cancellation.
    let _released = tokio::net::TcpListener::bind(address).await.unwrap();
}

#[tokio::test]
async fn directory_redirect_preserves_encoded_path_and_query() {
    let dir = TempDir::new().unwrap();
    for name in ["nested", "a b%", "nested/deep"] {
        fs::create_dir_all(dir.path().join(name)).unwrap();
        fs::write(dir.path().join(name).join("child.txt"), "child").unwrap();
        fs::write(
            dir.path().join(name).join("index.html"),
            "<script src=\"child.txt\"></script>",
        )
        .unwrap();
    }
    for index in [false, true] {
        for cors in [false, true] {
            for path in ["/nested", "/nested/deep", "/a%20b%25"] {
                for query in ["", "?", "?q=a%20b&x=%25"] {
                    let url = format!("{path}{query}");
                    let (status, headers, _) = request(dir.path(), &url, index, cors).await;
                    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT, "{url}");
                    assert_eq!(headers["location"], format!("{path}/{query}"));
                    assert_eq!(headers.contains_key("access-control-allow-origin"), cors);
                    let base = reqwest::Url::parse("http://localhost")
                        .unwrap()
                        .join(headers["location"].to_str().unwrap())
                        .unwrap();
                    let (status, _, body) = request(dir.path(), base.path(), index, cors).await;
                    assert_eq!(status, StatusCode::OK);
                    if index {
                        assert!(body.contains("src=\"child.txt\""));
                    } else {
                        assert!(body.contains("href=\"child.txt\""));
                    }
                    let asset = base.join("child.txt").unwrap();
                    assert_eq!(
                        request(dir.path(), asset.path(), index, cors).await.2,
                        "child"
                    );
                }
            }
            assert_eq!(
                request(dir.path(), "/", index, cors).await.0,
                StatusCode::OK
            );
            let (status, headers, _) = request(dir.path(), "/nested/child.txt", index, cors).await;
            assert_eq!(status, StatusCode::OK);
            assert!(!headers.contains_key("location"));
        }
    }
    let (_, _, listing) = request(dir.path(), "/", false, false).await;
    assert!(listing.contains("href=\"nested/\""));
    assert!(listing.contains("href=\"a%20b%25/\""));
    let (_, _, listing) = request(dir.path(), "/nested/", false, false).await;
    assert!(listing.contains("href=\"deep/\""));
}

#[cfg(unix)]
#[tokio::test]
async fn directory_redirect_never_exposes_outside_symlinks_or_traversal() {
    use std::os::unix::fs::symlink;
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("root");
    let outside = dir.path().join("outside");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&outside).unwrap();
    symlink(&outside, root.join("escape")).unwrap();
    for index in [false, true] {
        for path in [
            "/escape",
            "/escape/",
            "/%2e%2e/outside",
            "/%2e%2e%2foutside",
        ] {
            let (status, headers, _) = request(&root, path, index, false).await;
            assert_eq!(status, StatusCode::FORBIDDEN);
            assert!(!headers.contains_key("location"));
        }
    }
}

#[tokio::test]
async fn directory_redirect_cannot_become_a_network_path_reference() {
    let dir = TempDir::new().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let path = format!("/{}", root.display());
    let (status, headers, _) = request(&root, &path, false, false).await;
    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
    let base = reqwest::Url::parse("http://localhost/").unwrap();
    let redirect = base.join(headers["location"].to_str().unwrap()).unwrap();
    assert_eq!(redirect.host_str(), Some("localhost"));
    assert_eq!(
        request(&root, redirect.path(), false, false).await.0,
        StatusCode::OK
    );
}
