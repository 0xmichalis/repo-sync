use std::{path::PathBuf, sync::Arc};

use axum::{
    Json, Router,
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use chrono::Utc;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::{fs, sync::RwLock};

use crate::{config::AppConfig, path_guard::resolve_under_root, sync::SyncStatus};

#[derive(Clone)]
pub struct AppState {
    pub config: AppConfig,
    pub status: Arc<RwLock<SyncStatus>>,
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    current_sha: Option<String>,
    last_success_at: Option<chrono::DateTime<chrono::Utc>>,
    last_error: Option<String>,
}

#[derive(Serialize)]
struct MetaResponse {
    synced_repo_url: String,
    branch: String,
    serve_root: String,
    sync_interval_seconds: u64,
    now: chrono::DateTime<Utc>,
    sync: SyncStatus,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/health", get(health))
        .route("/meta", get(meta))
        .route("/files/*path", get(get_file))
        .with_state(state)
}

async fn index() -> impl IntoResponse {
    Json(serde_json::json!({
        "name": "repo-sync",
        "endpoints": ["/health", "/meta", "/files/*path"]
    }))
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let status = state.status.read().await.clone();
    let service_status = if status.last_error.is_some() && status.last_success_at.is_none() {
        "degraded"
    } else {
        "ok"
    };
    Json(HealthResponse {
        status: service_status,
        current_sha: status.current_sha,
        last_success_at: status.last_success_at,
        last_error: status.last_error,
    })
}

async fn meta(State(state): State<AppState>) -> impl IntoResponse {
    let status = state.status.read().await.clone();
    Json(MetaResponse {
        synced_repo_url: state.config.git_repo_url.clone(),
        branch: state.config.git_branch.clone(),
        serve_root: state.config.serve_root().to_string_lossy().to_string(),
        sync_interval_seconds: state.config.git_sync_interval_seconds,
        now: Utc::now(),
        sync: status,
    })
}

async fn get_file(
    State(state): State<AppState>,
    Path(path): Path<String>,
    headers: HeaderMap,
) -> Response {
    if path.len() > state.config.max_path_length {
        return (
            StatusCode::URI_TOO_LONG,
            Json(ErrorResponse {
                error: "path too long".to_string(),
            }),
        )
            .into_response();
    }

    let serve_root = state.config.serve_root();
    let file_path = match resolve_under_root(&serve_root, &path) {
        Ok(p) => p,
        Err(_) => {
            return (
                StatusCode::FORBIDDEN,
                Json(ErrorResponse {
                    error: "invalid path".to_string(),
                }),
            )
                .into_response();
        }
    };

    serve_file(file_path, headers, state.config.max_file_size_bytes).await
}

async fn serve_file(file_path: PathBuf, headers: HeaderMap, max_size: u64) -> Response {
    let metadata = match fs::metadata(&file_path).await {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: "file not found".to_string(),
                }),
            )
                .into_response();
        }
    };

    if !metadata.is_file() {
        return (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "not a file".to_string(),
            }),
        )
            .into_response();
    }
    if metadata.len() > max_size {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(ErrorResponse {
                error: "file exceeds max size".to_string(),
            }),
        )
            .into_response();
    }

    let bytes = match fs::read(&file_path).await {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: "failed to read file".to_string(),
                }),
            )
                .into_response();
        }
    };

    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let digest = hex::encode(hasher.finalize());
    let etag = format!("\"{digest}\"");

    if let Some(client_etag) = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        && client_etag == etag
    {
        return StatusCode::NOT_MODIFIED.into_response();
    }

    let content_type = mime_guess::from_path(&file_path).first_or_octet_stream();
    let mut response = Response::new(Body::from(bytes));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(content_type.as_ref())
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&etag).unwrap_or_else(|_| HeaderValue::from_static("\"invalid\"")),
    );
    if let Ok(modified) = metadata.modified() {
        let last_modified = httpdate::fmt_http_date(modified);
        if let Ok(v) = HeaderValue::from_str(&last_modified) {
            response.headers_mut().insert(header::LAST_MODIFIED, v);
        }
    }

    response
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use axum::{
        body::to_bytes,
        http::{Request, StatusCode, header},
    };
    use tempfile::tempdir;
    use tokio::sync::RwLock;
    use tower::ServiceExt;

    use crate::{config::AppConfig, sync::SyncStatus};

    use super::{AppState, router};

    #[tokio::test]
    async fn file_serving_reflects_file_update_without_restart() {
        let temp = tempdir().expect("temp dir");
        let mirror = temp.path().join("repo");
        std::fs::create_dir_all(&mirror).expect("create repo dir");
        std::fs::write(mirror.join("a.txt"), "one").expect("write file");

        let state = AppState {
            config: AppConfig {
                git_repo_url: "https://github.com/org/repo.git".to_string(),
                git_branch: "main".to_string(),
                git_sync_interval_seconds: 30,
                git_token: None,
                mirror_dir: mirror,
                serve_subdir: None,
                http_bind_addr: "127.0.0.1:0".to_string(),
                max_path_length: 512,
                max_file_size_bytes: 1024 * 1024,
            },
            status: Arc::new(RwLock::new(SyncStatus::default())),
        };
        let app = router(state);

        let first = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/files/a.txt")
                    .body(axum::body::Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(first.status(), StatusCode::OK);
        let first_body = to_bytes(first.into_body(), usize::MAX)
            .await
            .expect("read body");
        assert_eq!(first_body.as_ref(), b"one");

        tokio::time::sleep(Duration::from_millis(10)).await;
        std::fs::write(temp.path().join("repo").join("a.txt"), "two").expect("rewrite file");

        let second = app
            .oneshot(
                Request::builder()
                    .uri("/files/a.txt")
                    .body(axum::body::Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(second.status(), StatusCode::OK);
        let second_body = to_bytes(second.into_body(), usize::MAX)
            .await
            .expect("read body");
        assert_eq!(second_body.as_ref(), b"two");
    }

    #[tokio::test]
    async fn responds_not_modified_when_etag_matches() {
        let temp = tempdir().expect("temp dir");
        let mirror = temp.path().join("repo");
        std::fs::create_dir_all(&mirror).expect("create repo dir");
        std::fs::write(mirror.join("a.txt"), "same").expect("write file");

        let state = AppState {
            config: AppConfig {
                git_repo_url: "https://github.com/org/repo.git".to_string(),
                git_branch: "main".to_string(),
                git_sync_interval_seconds: 30,
                git_token: None,
                mirror_dir: mirror,
                serve_subdir: None,
                http_bind_addr: "127.0.0.1:0".to_string(),
                max_path_length: 512,
                max_file_size_bytes: 1024 * 1024,
            },
            status: Arc::new(RwLock::new(SyncStatus::default())),
        };
        let app = router(state);

        let first = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/files/a.txt")
                    .body(axum::body::Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(first.status(), StatusCode::OK);
        let etag = first
            .headers()
            .get(header::ETAG)
            .and_then(|v| v.to_str().ok())
            .expect("etag")
            .to_string();

        let second = app
            .oneshot(
                Request::builder()
                    .uri("/files/a.txt")
                    .header(header::IF_NONE_MATCH, etag)
                    .body(axum::body::Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(second.status(), StatusCode::NOT_MODIFIED);
    }
}
