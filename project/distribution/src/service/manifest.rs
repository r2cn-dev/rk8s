use std::{collections::HashMap, env, str::FromStr, sync::Arc};

use crate::utils::{
    state::AppState,
    validation::{is_valid_digest, is_valid_name, is_valid_reference},
};
use axum::{
    body::Body,
    extract::{Path, Query, Request, State},
    http::{header, Response, StatusCode},
    response::IntoResponse,
};
use futures::StreamExt;
use oci_spec::{distribution::TagListBuilder, image::Digest as oci_digest};
use oci_spec::{
    distribution::{ErrorCode, ErrorInfoBuilder, ErrorResponseBuilder},
    image::ImageManifest,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio_util::io::ReaderStream;

pub(crate) async fn get_manifest_handler(
    State(state): State<Arc<AppState>>,
    Path((name, reference)): Path<(String, String)>,
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
    if !is_valid_reference(&reference) {
        let error_info = ErrorInfoBuilder::default()
            .code(ErrorCode::Unsupported)
            .message("Invalid reference")
            .detail(json!({"reference": reference}).to_string())
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

    if is_valid_digest(&reference) {
        // the reference is a digest
        let digest = oci_digest::from_str(&reference).unwrap();
        match state.storage.read_by_digest(&digest).await {
            Ok(file) => {
                let image_manifest =
                    ImageManifest::from_reader(file.try_into_std().unwrap()).unwrap();
                let response = image_manifest.to_string().unwrap();
                return Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(response))
                    .unwrap();
            }
            Err(_) => {
                let error_info = ErrorInfoBuilder::default()
                    .code(ErrorCode::ManifestUnknown)
                    .message("Manifest unknown")
                    .detail(json!({"name": name, "reference": reference}).to_string())
                    .build()
                    .unwrap();
                let error_response = ErrorResponseBuilder::default()
                    .errors(vec![error_info])
                    .build()
                    .unwrap();
                return Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Body::from(serde_json::to_string(&error_response).unwrap()))
                    .unwrap();
            }
        }
    } else {
        // the reference is a tag
        match state.storage.read_by_tag(&name, &reference).await {
            Ok(file) => {
                let image_manifest =
                    ImageManifest::from_reader(file.try_into_std().unwrap()).unwrap();
                let response = image_manifest.to_string().unwrap();
                return Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(response))
                    .unwrap();
            }
            Err(_) => {
                let error_info = ErrorInfoBuilder::default()
                    .code(ErrorCode::ManifestUnknown)
                    .message("Manifest unknown")
                    .detail(json!({"name": name, "reference": reference}).to_string())
                    .build()
                    .unwrap();
                let error_response = ErrorResponseBuilder::default()
                    .errors(vec![error_info])
                    .build()
                    .unwrap();
                return Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Body::from(serde_json::to_string(&error_response).unwrap()))
                    .unwrap();
            }
        }
    }
}

pub(crate) async fn head_manifest_handler(
    State(state): State<Arc<AppState>>,
    Path((name, reference)): Path<(String, String)>,
) -> impl IntoResponse {
    if !is_valid_name(&name) {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::empty())
            .unwrap();
    }
    if !is_valid_reference(&reference) {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::empty())
            .unwrap();
    }

    if is_valid_digest(&reference) {
        // the reference is a digest
        let digest = oci_digest::from_str(&reference).unwrap();
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
                    .header(header::CONTENT_LENGTH, content_length.to_string())
                    .header("Docker-Content-Digest", digest_str)
                    .body(Body::empty())
                    .unwrap()
            }
            Err(_) => Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::empty())
                .unwrap(),
        }
    } else {
        // the reference is a tag
        match state.storage.read_by_tag(&name, &reference).await {
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
                    .header(header::CONTENT_LENGTH, content_length.to_string())
                    .header("Docker-Content-Digest", digest_str)
                    .body(Body::empty())
                    .unwrap()
            }
            Err(_) => Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::empty())
                .unwrap(),
        }
    }
}

pub async fn put_manifest_handler(
    State(state): State<Arc<AppState>>,
    Path((name, reference)): Path<(String, String)>,
    request: Request,
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
    if !is_valid_reference(&reference) {
        let error_info = ErrorInfoBuilder::default()
            .code(ErrorCode::Unsupported)
            .message("Invalid reference")
            .detail(json!({"reference": reference}).to_string())
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

    // Save the manifest to the storage
    let uuid = uuid::Uuid::new_v4().to_string();
    if let Err(_) = state
        .storage
        .write_by_uuid(&uuid, request.into_body().into_data_stream(), false)
        .await
    {
        return Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(axum::body::Body::empty())
            .unwrap();
    }

    let _digest = oci_digest::from_str(&reference);
    let digest_str;
    let is_tag;

    if is_valid_digest(&reference) {
        // the reference is a digest
        is_tag = false;
        digest_str = reference.clone();
    } else {
        // the reference is a tag
        is_tag = true;
        let file = state.storage.read_by_uuid(&uuid).await.unwrap();
        let stream = ReaderStream::new(file);
        let mut sha2_digest = Sha256::new();
        let mut stream = stream.fuse();
        while let Some(Ok(chunk)) = stream.next().await {
            sha2_digest.update(&chunk);
        }

        let sha2_digest = sha2_digest.finalize();
        digest_str = format!("sha256:{}", hex::encode(sha2_digest));
    }

    let digest = oci_digest::from_str(&digest_str).unwrap();

    if let Err(_) = state.storage.move_to_digest(&uuid, &digest).await {
        return Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(axum::body::Body::empty())
            .unwrap();
    }

    if is_tag {
        if let Err(_) = state.storage.link_to_tag(&name, &reference, &digest).await {
            return Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(axum::body::Body::empty())
                .unwrap();
        }
    }

    let location = format!("/v2/{}/manifests/{}", name.replace("/", "%2F"), digest);

    return Response::builder()
        .status(StatusCode::CREATED)
        .header(header::LOCATION, location)
        .body(Body::empty())
        .unwrap();
}

pub async fn get_tag_list_handler(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    match state.storage.walk_repo_dir(&name).await {
        Ok(mut tags) => {
            let has_param_n = params
                .get("n")
                .and_then(|value| value.parse::<usize>().ok())
                .is_some();
            let has_param_last = params.get("last").is_some();
            let mut need_link = false;
            let mut n_value = 0;
            if has_param_n {
                let n = params.get("n").unwrap().parse::<usize>().unwrap();
                if has_param_last {
                    let last = params.get("last").unwrap().parse::<String>().unwrap();
                    let last_index = tags.iter().position(|x| x == &last).unwrap();
                    tags = tags.split_off(last_index + 1);
                }
                need_link = n > 0 && tags.len() > n;
                n_value = n;
                tags.truncate(n);
            } else if has_param_last {
                let last = params.get("last").unwrap().parse::<String>().unwrap();
                let last_index = tags.iter().position(|x| x == &last).unwrap();
                tags = tags.split_off(last_index + 1);
            }
            let tag_list = TagListBuilder::default()
                .name(&name)
                .tags(tags)
                .build()
                .unwrap();

            let mut response = Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_string(&tag_list).unwrap()))
                .unwrap();

            if need_link {
                // TODO: get the registry URL from the environment
                let url = env::var("OCI_REGISTRY_URL").unwrap_or_else(|_| "127.0.0.1".to_string());
                let port = env::var("OCI_REGISTRY_PORT").unwrap_or_else(|_| "8968".to_string());
                let next_link = format!(
                    "http://{}:{}/v2/{}/tags/list?n={}&last={}",
                    url,
                    port,
                    name,
                    n_value,
                    tag_list.tags().last().unwrap()
                );
                let link_header = format!(r#"<{}>; rel="next""#, next_link);
                response
                    .headers_mut()
                    .insert(header::LINK, header::HeaderValue::from_str(&link_header).unwrap());
            }
            return response;
        }
        Err(_) => {
            return Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::empty())
                .unwrap();
        }
    }
}

pub async fn delete_manifest_handler(
    State(state): State<Arc<AppState>>,
    Path((name, reference)): Path<(String, String)>,
) -> impl IntoResponse {
    if !is_valid_name(&name) {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap();
    }
    if !is_valid_reference(&reference) {
        return Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap();
    }
    if is_valid_digest(&reference) {
        let digest = oci_digest::from_str(&reference).unwrap();
        match state.storage.delete_by_digest(&digest).await {
            Ok(_) => {
                return Response::builder()
                    .status(StatusCode::ACCEPTED)
                    .body(Body::empty())
                    .unwrap();
            }
            Err(_) => {
                return Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .body(Body::empty())
                    .unwrap();
            }
        }
    } else {
        match state.storage.delete_by_tag(&name, &reference).await {
            Ok(_) => {
                return Response::builder()
                    .status(StatusCode::ACCEPTED)
                    .body(Body::empty())
                    .unwrap();
            }
            Err(_) => {
                return Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .body(Body::empty())
                    .unwrap();
            }
        }
    }
}
