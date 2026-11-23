//! Corvin game server binary entrypoint.

#![forbid(unsafe_code)]

use std::env;
use std::net::SocketAddr;

use tokio::net::TcpListener;

#[tokio::main]
async fn main() {
    let host = env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_owned());
    let port: u16 = env::var("PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(3000);

    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], port)));

    let listener = TcpListener::bind(addr)
        .await
        .unwrap_or_else(|err| panic!("failed to bind {addr}: {err}"));
    let bound = listener
        .local_addr()
        .unwrap_or_else(|err| panic!("failed to read bound address: {err}"));
    eprintln!("corvin-server listening on http://{bound}");

    axum::serve(listener, corvin_server::app())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap_or_else(|err| panic!("server error: {err}"));
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{signal, SignalKind};
        signal(SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
