use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Query, Request, State};
use axum::http::header::{HeaderMap, LOCATION, RANGE};
use axum::http::{header, Response};
use axum::response::IntoResponse;
use axum::{extract::Path, http::StatusCode};
use futures::StreamExt;
use oci_spec::distribution::{ErrorCode, ErrorInfoBuilder, ErrorResponseBuilder};
use oci_spec::image::Digest as oci_digest;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio_util::io::ReaderStream;

use crate::utils::state::AppState;
use crate::utils::validation::{ is_valid_name, is_valid_range};

pub(crate) async fn get_blob_handler(
    State(state): State<Arc<AppState>>,
    Path((name, digest)): Path<(String, String)>,
) -> impl IntoResponse {
    if !is_valid_name(&name) {
        let error_info = ErrorInfoBuilder::default()
            .code(ErrorCode::NameInvalid)
            .message("Invalid name")
            .detail(json!({"name": name}).to_string())
            .build()
            .unwrap();
        let error_response = ErrorResponseBuilder::default()
            .errors(vec![error_info])
            .build()
            .unwrap();

        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_string(&error_response).unwrap()))
            .unwrap();
    }

    let _digest = oci_digest::from_str(&digest);
    if !_digest.is_ok() {
        let error_info = ErrorInfoBuilder::default()
            .code(ErrorCode::DigestInvalid)
            .message("Invalid digest")
            .detail(json!({"digest": digest}).to_string())
            .build()
            .unwrap();
        let error_response = ErrorResponseBuilder::default()
            .errors(vec![error_info])
            .build()
            .unwrap();

        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_string(&error_response).unwrap()))
            .unwrap();
    }
    let digest = _digest.unwrap();

    match state.storage.read_by_digest( &digest).await {
        Ok(file) => {
            let file_stream = ReaderStream::new(file);
            let responese = Body::from_stream(file_stream);

            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .body(responese)
                .unwrap()
        }
        Err(_) => {
            let error_info = ErrorInfoBuilder::default()
                .code(ErrorCode::BlobUnknown)
                .message("Blob unknown")
                .detail(json!({"name": name, "digest": digest}).to_string())
                .build()
                .unwrap();
            let error_response = ErrorResponseBuilder::default()
                .errors(vec![error_info])
                .build()
                .unwrap();

            Response::builder()
                .status(StatusCode::NOT_FOUND)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&error_response).unwrap()))
                .unwrap()
        }
    }
}

pub(crate) async fn head_blob_handler(
    State(state): State<Arc<AppState>>,
    Path((name, digest)): Path<(String, String)>,
) -> impl IntoResponse {
    if !is_valid_name(&name) {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::empty())
            .unwrap();
    }

    let _digest = oci_digest::from_str(&digest);
    if !_digest.is_ok() {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::empty())
            .unwrap();
    }
    let digest = _digest.unwrap();

    match state.storage.read_by_digest(&digest).await {
        Ok(file) => {
            let stream = ReaderStream::new(file);
            let mut digest = Sha256::new();
            let mut content_length = 0;

            let mut stream = stream.fuse();
            while let Some(Ok(chunk)) = stream.next().await {
                digest.update(&chunk);
                content_length += chunk.len();
            }

            let digest = digest.finalize();
            let digest_str = format!("sha256:{}", hex::encode(digest));

            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::CONTENT_LENGTH, content_length.to_string())
                .header("Docker-Content-Digest", digest_str)
                .body(Body::empty())
                .unwrap()
        }
        Err(_) => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header("Content-Type", "application/json")
            .body(Body::empty())
            .unwrap(),
    }
}

// Open a blob upload session
pub async fn post_blob_handler(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    request: Request,
) -> impl IntoResponse {
    let digest = params.get("digest").unwrap_or(&"".to_string()).clone();
    // TODO: Support mount and from parameters
    if digest.is_empty() {
        // Obtain a session id (upload URL)
        match state.create_session().await {
            Ok(session_id) => {
                let location = format!(
                    "/v2/{}/blobs/uploads/{}",
                    name.replace("/", "%2F"),
                    session_id
                );

                Response::builder()
                    .status(StatusCode::ACCEPTED)
                    .header(header::LOCATION, location)
                    .body(axum::body::Body::empty())
                    .unwrap()
            }
            Err(_) => Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(axum::body::Body::empty())
                .unwrap(),
        }
    } else {
        // Pushing a blob monolithically (Single POST)
        let content_length = headers
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);

        if content_length == 0 {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(axum::body::Body::empty())
                .unwrap();
        }

        let _digest = oci_digest::from_str(&digest);
        if !_digest.is_ok() {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(axum::body::Body::empty())
                .unwrap();
        }
        let digest = _digest.unwrap();

        match state
            .storage
            .write_by_digest(
                &digest,
                request.into_body().into_data_stream(),
                false,
            )
            .await
        {
            Ok(_) => {
                let location =
                    format!("/v2/{}/blobs/{}", name.replace("/", "%2F"), digest.digest());

                Response::builder()
                    .status(StatusCode::CREATED)
                    .header(header::LOCATION, location)
                    .body(axum::body::Body::empty())
                    .unwrap()
            }
            Err(_) => Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(axum::body::Body::empty())
                .unwrap(),
        }
    }
}

// Close a blob upload session
pub async fn put_blob_handler(
    State(state): State<Arc<AppState>>,
    Path((name, session_id)): Path<(String, String)>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    request: Request,
) -> impl IntoResponse {
    // Invalid PUT request without digest
    let digest = params.get("digest").unwrap_or(&"".to_string()).clone();
    if digest.is_empty() {
        return Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(axum::body::Body::empty())
            .unwrap();
    }

    let _digest = oci_digest::from_str(&digest);
    if !_digest.is_ok() {
        return Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(axum::body::Body::empty())
            .unwrap();
    }
    let digest = _digest.unwrap();
    let content_length = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);

    if let Some(_) = state.get_session(&session_id).await {
        state.update_session(&session_id, content_length).await;
        let location = format!("/v2/{}/blobs/{}", name.replace("/", "%2F"), digest.digest());

        // Save the final chunk
        if content_length != 0 {
            if let Err(_) = state
                .storage
                .write_by_uuid(
                    &session_id,
                    request.into_body().into_data_stream(),
                    false,
                )
                .await
            {
                return Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(axum::body::Body::empty())
                    .unwrap();
            }
        }

        if let Err(_) = state.storage.move_to_digest(&session_id, &digest).await {
            return Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(axum::body::Body::empty())
                .unwrap();
        }

        state.close_session(&session_id).await;

        return Response::builder()
            .status(StatusCode::CREATED)
            .header(header::LOCATION, location)
            .body(axum::body::Body::empty())
            .unwrap();
    } else {
        // Session not found
        return Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(axum::body::Body::empty())
            .unwrap();
    }
}

// Pushing a blob in chunks
pub async fn patch_blob_handler(
    State(state): State<Arc<AppState>>,
    Path((name, session_id)): Path<(String, String)>,
    headers: HeaderMap,
    request: Request,
) -> impl IntoResponse {
    let content_length = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let content_range = headers
        .get(header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if content_length == 0 || !is_valid_range(content_range) {
        return Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(axum::body::Body::empty())
            .unwrap();
    }

    if let Some(session) = state.get_session(&session_id).await {
        let range_begin = content_range.split('-').collect::<Vec<&str>>()[0]
            .parse::<u64>()
            .unwrap();
        if range_begin < session.uploaded
            || range_begin
                != (if session.uploaded == 0 {
                    0
                } else {
                    session.uploaded + 1
                })
        {
            return Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .body(axum::body::Body::empty())
                .unwrap();
        }

        state.update_session(&session_id, content_length).await;
        let location = format!(
            "/v2/{}/blobs/uploads/{}",
            name.replace("/", "%2F"),
            session_id
        );
        let end_of_range = state.get_session(&session_id).await.unwrap().uploaded;

        match state
            .storage
            .write_by_uuid(&session_id, request.into_body().into_data_stream(), true)
            .await
        {
            Ok(_) => {
                return Response::builder()
                    .status(StatusCode::ACCEPTED)
                    .header(LOCATION, location)
                    .header(RANGE, format!("0-{}", end_of_range))
                    .body(axum::body::Body::empty())
                    .unwrap();
            }
            Err(_) => {
                return Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(axum::body::Body::empty())
                    .unwrap();
            }
        }
    } else {
        // Session not found
        return Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(axum::body::Body::empty())
            .unwrap();
    }
}

// Get the status of upload session
pub async fn get_blob_status_handler(
    State(state): State<Arc<AppState>>,
    Path((name, session_id)): Path<(String, String)>,
) -> impl IntoResponse {
    if let Some(session) = state.get_session(&session_id).await {
        let location = format!(
            "/v2/{}/blobs/uploads/{}",
            name.replace("/", "%2F"),
            session_id
        );
        let end_of_range = session.uploaded;

        Response::builder()
            .status(StatusCode::NO_CONTENT)
            .header(LOCATION, location)
            .header(RANGE, format!("0-{}", end_of_range))
            .body(axum::body::Body::empty())
            .unwrap()
    } else {
        // Session not found
        Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(axum::body::Body::empty())
            .unwrap()
    }
}

pub async fn delete_blob_handler(
    State(state): State<Arc<AppState>>,
    Path((_name, digest)): Path<(String, String)>,
) -> impl IntoResponse {
    let _digest = oci_digest::from_str(&digest);
    if !_digest.is_ok() {
        return Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(axum::body::Body::empty())
            .unwrap();
    }
    let digest = _digest.unwrap();

    match state.storage.delete_by_digest(&digest).await {
        Ok(_) => Response::builder()
            .status(StatusCode::ACCEPTED)
            .body(axum::body::Body::empty())
            .unwrap(),
        Err(_) => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(axum::body::Body::empty())
            .unwrap(),
    }
}