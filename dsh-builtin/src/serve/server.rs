use super::config::ServeConfig;
use super::error::ServeError;
use super::handlers::{serve_file_handler, serve_root_handler};
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

        let listener = TcpListener::bind(&addr)
            .await
            .map_err(|e| ServeError::NetworkError(format!("Failed to bind to {addr}: {e}")))?;

        info!("HTTP server listening on http://{}", addr);

        // Create a router with proper file serving capabilities
        let serve_dir = self.config.directory.clone();
        let serve_index = self.config.serve_index;
        let enable_cors = self.config.enable_cors;

        let app = Router::new()
            .route(
                "/",
                get({
                    let serve_dir = serve_dir.clone();
                    move || serve_root_handler(serve_dir, serve_index)
                }),
            )
            .route(
                "/*path",
                get({
                    let serve_dir = serve_dir.clone();
                    move |uri: Uri| serve_file_handler(uri, serve_dir, serve_index, enable_cors)
                }),
            );

        axum::serve(listener, app)
            .await
            .map_err(|e| ServeError::ServerError(Box::new(e)))?;

        Ok(())
    }
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
            let mut sigterm = signal::unix::signal(signal::unix::SignalKind::terminate())
                .expect("failed to install SIGTERM handler");
            let mut sigint = signal::unix::signal(signal::unix::SignalKind::interrupt())
                .expect("failed to install SIGINT handler");

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
    let server_task = tokio::spawn(async move {
        if let Err(e) = server.start().await {
            error!("server error: {}", e);
        }
    });

    // Wait for shutdown signal
    let shutdown_task = tokio::spawn(async {
        SignalHandler::wait_for_shutdown().await;
    });

    // Wait for either server completion or shutdown signal
    tokio::select! {
        result = server_task => {
            match result {
                Ok(_) => {
                    info!("server completed successfully");
                }
                Err(e) => {
                    error!("server task failed: {}", e);
                    return Err(ServeError::ServerError(Box::new(e)));
                }
            }
        }
        _ = shutdown_task => {
            info!("shutdown signal received");
        }
    }

    info!("HTTP server stopped");
    Ok(())
}
