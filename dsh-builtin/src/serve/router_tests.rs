use super::{config::ServeConfig, server::build_router};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use std::{fs, path::Path};
use tempfile::TempDir;
use tower::ServiceExt;

#[tokio::test]
async fn production_serve_router_rejects_rebinding_hosts() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("index.html"), "private sentinel").unwrap();
    for cors in [false, true] {
        let app = build_router(&ServeConfig {
            directory: dir.path().to_owned(),
            enable_cors: cors,
            ..ServeConfig::default()
        })
        .unwrap();
        for host in [
            "attacker.example:8000",
            "localhost.attacker.example",
            "127.0.0.1.attacker.example",
            "attacker@localhost:8000",
            "localhost:invalid",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/")
                        .header("host", host)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{host}");
            let body = to_bytes(response.into_body(), 1024).await.unwrap();
            assert!(!String::from_utf8_lossy(&body).contains("private sentinel"));
        }
        for host in ["127.0.0.1:8000", "localhost:8000", "LOCALHOST:8000"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/")
                        .header("host", host)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{host}");
        }
    }
}

#[tokio::test]
async fn production_serve_router_checks_all_authority_sources() {
    use axum::http::Version;
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("index.html"), "safe").unwrap();
    let app = build_router(&ServeConfig {
        directory: dir.path().to_owned(),
        ..ServeConfig::default()
    })
    .unwrap();
    let cases = [
        (Request::builder().uri("/"), StatusCode::FORBIDDEN),
        (
            Request::builder().uri("/").version(Version::HTTP_10),
            StatusCode::OK,
        ),
        (
            Request::builder()
                .uri("/")
                .header("host", "localhost")
                .header("host", "attacker.example"),
            StatusCode::FORBIDDEN,
        ),
        (
            Request::builder()
                .uri("http://attacker.example/")
                .header("host", "localhost"),
            StatusCode::FORBIDDEN,
        ),
        (
            Request::builder()
                .uri("http://localhost/")
                .header("host", "attacker.example"),
            StatusCode::FORBIDDEN,
        ),
        (
            Request::builder()
                .uri("http://localhost/")
                .version(Version::HTTP_2),
            StatusCode::OK,
        ),
        (
            Request::builder()
                .uri("http://attacker.example/")
                .version(Version::HTTP_2),
            StatusCode::FORBIDDEN,
        ),
    ];
    for (request, expected) in cases {
        let response = app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
}

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
    })
    .unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .uri(path)
                .header("host", "localhost:8000")
                .body(Body::empty())
                .unwrap(),
        )
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
            assert_eq!(status, StatusCode::NOT_FOUND);
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
        assert_eq!(status, StatusCode::NOT_FOUND);
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
    })
    .unwrap();
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
            assert_eq!(status, StatusCode::NOT_FOUND);
            assert!(!headers.contains_key("location"));
        }
    }
}

#[tokio::test]
async fn directory_redirect_cannot_become_a_network_path_reference() {
    let dir = TempDir::new().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    // Backslash is a filename character on both supported Unix platforms.
    let directory_name = std::ffi::OsStr::new(r"\example.com");
    fs::create_dir(root.join(directory_name)).unwrap();
    // A `//`-prefixed spelling decodes to an absolute path, which is refused
    // before any redirect is built - so it can carry no Location at all.
    let (status, headers, _) = request(&root, &format!("/{}", root.display()), false, false).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!headers.contains_key("location"));
    let (status, headers, _) = request(&root, r"/\example.com", false, false).await;
    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
    let base = reqwest::Url::parse("http://localhost/").unwrap();
    let redirect = base.join(headers["location"].to_str().unwrap()).unwrap();
    assert_eq!(redirect.host_str(), Some("localhost"));
    assert_eq!(
        request(&root, redirect.path(), false, false).await.0,
        StatusCode::OK
    );
}

/// A decoded absolute path used to be joined as-is (`join` replaces the base),
/// so `/%2Fetc%2Fshadow` answered 403 and `/%2Fetc%2Fnope` 404 - an existence
/// oracle for the whole filesystem. Anything outside the root is now one 404.
#[tokio::test]
async fn production_serve_router_has_no_existence_oracle_outside_the_root() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("root");
    fs::create_dir(&root).unwrap();
    let secret = fs::canonicalize(dir.path()).unwrap().join("secret.txt");
    fs::write(&secret, "OUTSIDE_SENTINEL").unwrap();
    let missing = secret.with_file_name("missing.txt");
    for target in [&secret, &missing] {
        let raw = target.display().to_string();
        for url in [format!("/{}", raw.replace('/', "%2F")), format!("/{raw}")] {
            let (status, _, body) = request(&root, &url, true, false).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{url}");
            assert!(!body.contains("OUTSIDE_SENTINEL"));
        }
    }
}

#[tokio::test]
async fn production_serve_file_sets_nosniff() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("data.txt"), "plain").unwrap();
    let (status, headers, _) = request(dir.path(), "/data.txt", false, false).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["x-content-type-options"], "nosniff");
}

#[tokio::test]
async fn production_router_pins_file_and_index_streams_before_path_replacement() {
    use std::os::unix::fs::symlink;
    for index in [false, true] {
        let dir = TempDir::new().unwrap();
        let public = dir.path().join("public");
        fs::create_dir(&public).unwrap();
        let name = if index { "index.html" } else { "file.txt" };
        fs::write(public.join(name), "PUBLIC").unwrap();
        let outside = dir.path().join("secret");
        fs::write(&outside, "OUTSIDE_SENTINEL").unwrap();
        let app = build_router(&ServeConfig {
            directory: public.clone(),
            ..ServeConfig::default()
        })
        .unwrap();
        let response = app
            .oneshot(
                Request::builder()
                    .uri(if index { "/" } else { "/file.txt" })
                    .header("host", "localhost")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        // The body has not been read yet. Streaming must retain the descriptor
        // opened at authorization rather than resolving a pathname on demand.
        fs::remove_file(public.join(name)).unwrap();
        symlink(outside, public.join(name)).unwrap();
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        assert_eq!(body, "PUBLIC");
    }
}

#[tokio::test]
async fn production_router_never_serves_a_replaced_public_root() {
    use std::os::unix::fs::symlink;
    for link in [false, true] {
        let dir = TempDir::new().unwrap();
        let public = dir.path().join("public");
        let outside = dir.path().join("outside");
        fs::create_dir(&public).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(public.join("file.txt"), "PUBLIC").unwrap();
        fs::write(outside.join("file.txt"), "OUTSIDE_SENTINEL").unwrap();
        fs::write(outside.join("private-name"), "private").unwrap();
        let app = build_router(&ServeConfig {
            directory: public.clone(),
            serve_index: false,
            ..ServeConfig::default()
        })
        .unwrap();
        fs::rename(&public, dir.path().join("original")).unwrap();
        if link {
            symlink(&outside, &public).unwrap();
        } else {
            fs::rename(&outside, &public).unwrap();
        }
        for uri in ["/file.txt", "/"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .header("host", "localhost")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                if link {
                    StatusCode::NOT_FOUND
                } else {
                    StatusCode::OK
                }
            );
            let body = to_bytes(response.into_body(), 8192).await.unwrap();
            let body = String::from_utf8_lossy(&body);
            assert!(!body.contains("OUTSIDE_SENTINEL") && !body.contains("private-name"));
        }
    }
}

#[tokio::test]
async fn no_index_lists_readable_directories_without_search_permission() {
    use std::os::unix::fs::PermissionsExt;
    for root_listing in [false, true] {
        let dir = TempDir::new().unwrap();
        let public = dir.path().join("public");
        let leaf = public.join("leaf");
        fs::create_dir_all(&leaf).unwrap();
        let limited = if root_listing { &public } else { &leaf };
        fs::write(
            limited.join("visible-name"),
            "not readable without search permission",
        )
        .unwrap();
        fs::set_permissions(limited, fs::Permissions::from_mode(0o400)).unwrap();
        assert!(fs::read_dir(limited).is_ok());
        let app = build_router(&ServeConfig {
            directory: public.clone(),
            serve_index: false,
            ..ServeConfig::default()
        });
        let response = match app {
            Ok(app) => Some(
                app.oneshot(
                    Request::builder()
                        .uri(if root_listing { "/" } else { "/leaf/" })
                        .header("host", "localhost")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
            ),
            Err(_) => None,
        };
        fs::set_permissions(limited, fs::Permissions::from_mode(0o700)).unwrap();
        let response = response.expect("readable public root must open");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 8192).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains("visible-name"));
    }
}
