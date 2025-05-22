use axum::{self, Json, Router, extract::Path, http::StatusCode, routing::get};
use serde_json::json;
use tracing::info;

use crate::{commands::load_container, rootpath};

pub async fn init() {
    let app = Router::new().route(
        "/containers/{container_id}/json",
        get(handle_container_request),
    );

    let addr = "0.0.0.0:10250";
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    info!("Serving on {}", addr);
    axum::serve(listener, app).await.unwrap();
}

async fn handle_container_request(
    Path(container_id): Path<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    let path = rootpath::determine(None);
    match path {
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "msg": e.to_string() })),
        ),
        Ok(p) => {
            let container = load_container(p, &container_id);
            match container {
                Ok(c) => (
                    StatusCode::OK,
                    Json(serde_json::to_value(&c.state).unwrap()),
                ),
                Err(e) => (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "msg": e.to_string() })),
                ),
            }
        }
    }
}
