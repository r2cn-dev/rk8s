pub mod v2;

use std::sync::Arc;

use axum::Router;

use crate::utils::state::AppState;

pub fn create_router() -> Router<Arc<AppState>> {
    Router::new().nest("/v2", v2::create_v2_router())
}
