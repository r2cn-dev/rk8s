use dotenv::dotenv;
use std::env;
use std::sync::Arc;
use tokio::signal;
use utils::state::AppState;

mod api;
mod service;
mod storage;
mod utils;

#[tokio::main]
async fn main() {
    dotenv().ok();

    let state = Arc::new(AppState::new());

    let app = api::create_router().with_state(state);

    let url = env::var("OCI_REGISTRY_URL").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = env::var("OCI_REGISTRY_PORT").unwrap_or_else(|_| "8968".to_string());

    let listener = tokio::net::TcpListener::bind(format!("{}:{}", url, port))
        .await
        .unwrap();
    println!("listening on {}", listener.local_addr().unwrap());

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    println!("Shutting down...");
}
