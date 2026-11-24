//! Corvin game server binary entrypoint.

#![forbid(unsafe_code)]

use std::env;
use std::net::SocketAddr;
use std::process::ExitCode;

use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("migrate") => match args.next().as_deref() {
            Some("up") => run_migrate_up().await,
            Some("down") => run_migrate_down().await,
            other => {
                eprintln!(
                    "usage: corvin-server migrate <up|down> (got {:?})",
                    other.unwrap_or("<missing>")
                );
                ExitCode::from(2)
            }
        },
        Some(other) => {
            eprintln!("unknown command {other:?}; usage: corvin-server [migrate <up|down>]");
            ExitCode::from(2)
        }
        None => match run_server().await {
            Ok(()) => ExitCode::SUCCESS,
            Err(code) => code,
        },
    }
}

async fn run_migrate_up() -> ExitCode {
    let pool = match corvin_server::db::connect_from_env().await {
        Ok(pool) => pool,
        Err(err) => {
            eprintln!("migrate up: connect failed: {err}");
            return ExitCode::FAILURE;
        }
    };
    match corvin_server::db::migrate_up(&pool).await {
        Ok(()) => {
            eprintln!("migrate up: ok");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("migrate up: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn run_migrate_down() -> ExitCode {
    let pool = match corvin_server::db::connect_from_env().await {
        Ok(pool) => pool,
        Err(err) => {
            eprintln!("migrate down: connect failed: {err}");
            return ExitCode::FAILURE;
        }
    };
    match corvin_server::db::migrate_down(&pool).await {
        Ok(()) => {
            eprintln!("migrate down: ok");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("migrate down: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn run_server() -> Result<(), ExitCode> {
    let host = env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_owned());
    let port: u16 = env::var("PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(3000);

    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], port)));

    let pool = corvin_server::db::connect_from_env().await.map_err(|err| {
        eprintln!("database connect failed: {err}");
        ExitCode::FAILURE
    })?;
    let state = corvin_server::AppState::from_env(pool).map_err(|err| {
        eprintln!("auth state failed: {err}");
        ExitCode::FAILURE
    })?;

    let listener = TcpListener::bind(addr).await.map_err(|err| {
        eprintln!("failed to bind {addr}: {err}");
        ExitCode::FAILURE
    })?;
    let bound = listener.local_addr().map_err(|err| {
        eprintln!("failed to read bound address: {err}");
        ExitCode::FAILURE
    })?;
    eprintln!("corvin-server listening on http://{bound}");

    axum::serve(listener, corvin_server::app(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|err| {
            eprintln!("server error: {err}");
            ExitCode::FAILURE
        })?;
    Ok(())
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
