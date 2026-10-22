use super::config::ServeConfig;
use super::error::ServeError;
use super::handlers::serve_file_handler;
use axum::{Router, http::Uri, routing::get};
use dsh_types::Context;
use tracing::{debug, error, info};

use std::net::SocketAddr;
use tokio::net::TcpListener;

/// HTTP server implementation for serving files
/// Handles server lifecycle, configuration, and graceful shutdown
pub struct HttpServer {
    config: ServeConfig,
}

impl HttpServer {
    /// Create a new HTTP server with the given configuration
    pub fn new(config: ServeConfig) -> Self {
        Self { config }
    }

    /// Start the HTTP server and handle requests
    pub async fn start(&mut self) -> Result<(), ServeError> {
        let addr: SocketAddr = format!("{}:{}", self.config.host, self.config.port)
            .parse()
            .map_err(|e| ServeError::NetworkError(format!("Invalid address: {e}")))?;

        let listener = TcpListener::bind(&addr).await.map_err(|source| {
            if source.kind() == std::io::ErrorKind::AddrInUse {
                ServeError::PortInUse {
                    port: self.config.port,
                }
            } else {
                ServeError::BindError {
                    addr: addr.to_string(),
                    source,
                }
            }
        })?;

        info!("HTTP server listening on http://{}", addr);

        let app = build_router(&self.config);

        axum::serve(listener, app)
            .await
            .map_err(|e| ServeError::ServerError(Box::new(e)))?;

        Ok(())
    }
}

/// Construct the router used by the HTTP server.
pub(super) fn build_router(config: &ServeConfig) -> Router {
    let serve_dir = config.directory.clone();
    let serve_index = config.serve_index;
    let enable_cors = config.enable_cors;

    Router::new()
        .route(
            "/",
            get({
                let serve_dir = serve_dir.clone();
                move |uri: Uri| serve_file_handler(uri, serve_dir, serve_index, enable_cors)
            }),
        )
        .route(
            "/{*path}",
            get({
                let serve_dir = serve_dir.clone();
                move |uri: Uri| serve_file_handler(uri, serve_dir, serve_index, enable_cors)
            }),
        )
}

/// Signal handler for graceful shutdown
/// Handles various shutdown signals across different platforms
pub struct SignalHandler;

impl SignalHandler {
    /// Wait for shutdown signal (Ctrl+C, SIGTERM, etc.)
    pub async fn wait_for_shutdown() {
        use tokio::signal;

        #[cfg(unix)]
        {
            let mut sigterm = match signal::unix::signal(signal::unix::SignalKind::terminate()) {
                Ok(sigterm) => sigterm,
                Err(err) => {
                    error!("failed to install SIGTERM handler: {}", err);
                    let _ = signal::ctrl_c().await;
                    info!("received Ctrl+C");
                    return;
                }
            };
            let mut sigint = match signal::unix::signal(signal::unix::SignalKind::interrupt()) {
                Ok(sigint) => sigint,
                Err(err) => {
                    error!("failed to install SIGINT handler: {}", err);
                    let _ = signal::ctrl_c().await;
                    info!("received Ctrl+C");
                    return;
                }
            };

            tokio::select! {
                _ = sigterm.recv() => {
                    info!("received SIGTERM");
                }
                _ = sigint.recv() => {
                    info!("received SIGINT");
                }
            }
        }

        #[cfg(windows)]
        {
            let _ = signal::ctrl_c().await;
            info!("received Ctrl+C");
        }
    }
}

/// Start the HTTP server with the given configuration
/// This function handles the complete server lifecycle using the existing runtime
pub fn start_http_server(ctx: &Context, config: ServeConfig) -> Result<(), ServeError> {
    debug!("starting HTTP server with configuration: {:?}", config);

    // Create a new runtime specifically for the server to avoid nesting issues
    // This is the safest approach when we need to run async code from sync context
    let server_rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            error!("failed to create server runtime: {}", e);
            return Err(ServeError::ServerError(Box::new(e)));
        }
    };

    // Use spawn_blocking to run the server in a separate thread
    // This completely avoids the runtime nesting issue
    let ctx_clone = ctx.clone();
    let result =
        std::thread::spawn(move || server_rt.block_on(run_server_async(&ctx_clone, config))).join();

    match result {
        Ok(server_result) => server_result,
        Err(e) => {
            error!("server thread panicked: {:?}", e);
            Err(ServeError::ServerError(Box::new(std::io::Error::other(
                "Server thread panicked",
            ))))
        }
    }
}

/// Async function to run the HTTP server
async fn run_server_async(ctx: &Context, config: ServeConfig) -> Result<(), ServeError> {
    debug!("running HTTP server asynchronously");

    // Create and start the HTTP server
    let mut server = HttpServer::new(config);

    // Display server information
    ctx.write_stdout("🐕 doge-shell HTTP server starting...")
        .ok();
    ctx.write_stdout(&format!(
        "  Directory: {}",
        server.config.directory.display()
    ))
    .ok();
    ctx.write_stdout(&format!("  Port: {}", server.config.port))
        .ok();
    ctx.write_stdout("  Press Ctrl+C to stop the server").ok();
    ctx.write_stdout("").ok();

    // Start the server in a background task
    let mut server_task = tokio::spawn(async move { server.start().await });

    // Wait for shutdown signal
    let mut shutdown_task = tokio::spawn(async {
        SignalHandler::wait_for_shutdown().await;
    });

    // Retain both task handles so the other task is cancelled and joined on exit.
    let result = tokio::select! {
        result = &mut server_task => {
            shutdown_task.abort();
            let _ = shutdown_task.await;
            result.map_err(|e| ServeError::ServerError(Box::new(e)))?
        }
        _ = &mut shutdown_task => {
            info!("shutdown signal received");
            server_task.abort();
            let _ = server_task.await;
            Ok(())
        }
    };

    info!("HTTP server stopped");
    result
}
