use std::sync::Arc;

use crate::service::blob::{
    delete_blob_handler, get_blob_handler, get_blob_status_handler, head_blob_handler,
    patch_blob_handler, post_blob_handler, put_blob_handler,
};
use crate::service::manifest::{
    delete_manifest_handler, get_manifest_handler, get_tag_list_handler, head_manifest_handler,
    put_manifest_handler,
};
use crate::utils::state::AppState;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, get, head, patch, post, put};
use axum::Router;

pub fn create_v2_router() -> Router<Arc<AppState>> {
    // NOTE: if the `name` contains a `/`, it should be encoded as `%2F` in requests and responses.
    Router::new()
        // Determine support
        .route("/", get(|| async { StatusCode::OK.into_response() }))
        // Pull manifests
        .route("/{name}/manifests/{reference}", get(get_manifest_handler))
        // Pull blobs
        .route("/{name}/blobs/{digest}", get(get_blob_handler))
        // Check if content exists in the registry
        .route("/{name}/manifests/{reference}", head(head_manifest_handler))
        .route("/{name}/blobs/{digest}", head(head_blob_handler))
        // Open a blob upload session
        .route("/{name}/blobs/uploads/", post(post_blob_handler))
        // Push a blob in chunks
        .route(
            "/{name}/blobs/uploads/{session_id}",
            patch(patch_blob_handler),
        )
        .route(
            "/{name}/blobs/uploads/{session_id}",
            get(get_blob_status_handler),
        )
        // Close a blob upload session
        .route("/{name}/blobs/uploads/{session_id}", put(put_blob_handler))
        // Push Manifests
        .route("/{name}/manifests/{reference}", put(put_manifest_handler))
        // List tags
        .route("/{name}/tags/list", get(get_tag_list_handler))
        // Delete manifests or tags
        .route(
            "/{name}/manifests/{reference}",
            delete(delete_manifest_handler),
        )
        // Delete blobs
        .route("/{name}/blobs/{digest}", delete(delete_blob_handler))
}
