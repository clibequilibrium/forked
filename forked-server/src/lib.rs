//! # forked-server
//!
//! The self-hostable forked server: axum in front of the shared
//! [`forked_api`] dispatcher, so it exposes exactly the same `/v1` API as
//! forked cloud. Storage is SQLite (a single file) or in-memory.

#![forbid(unsafe_code)]

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::Response;
use axum::Router;
use forked_api::ApiRequest;
use forked_core::{Forked, Hash, Storage, StorageError};
use forked_memory::MemoryStorage;
use forked_sqlite::SqliteStorage;
use tower_http::cors::CorsLayer;

/// Maximum accepted request body (16 MiB).
const BODY_LIMIT: usize = 16 * 1024 * 1024;

/// The storage backend chosen at startup.
///
/// A concrete enum (rather than a trait object) keeps the engine's futures
/// `Send`, which axum's multi-threaded runtime requires.
pub enum Backend {
    /// Volatile in-memory storage (dev/demo).
    Memory(MemoryStorage),
    /// SQLite file storage.
    Sqlite(SqliteStorage),
}

impl Storage for Backend {
    async fn get_blob(&self, hash: &Hash) -> Result<Option<Vec<u8>>, StorageError> {
        match self {
            Backend::Memory(s) => s.get_blob(hash).await,
            Backend::Sqlite(s) => s.get_blob(hash).await,
        }
    }

    async fn put_blob(&self, hash: &Hash, bytes: &[u8]) -> Result<(), StorageError> {
        match self {
            Backend::Memory(s) => s.put_blob(hash, bytes).await,
            Backend::Sqlite(s) => s.put_blob(hash, bytes).await,
        }
    }

    async fn get_ref(&self, name: &str) -> Result<Option<Hash>, StorageError> {
        match self {
            Backend::Memory(s) => s.get_ref(name).await,
            Backend::Sqlite(s) => s.get_ref(name).await,
        }
    }

    async fn cas_ref(
        &self,
        name: &str,
        expected: Option<&Hash>,
        new: &Hash,
    ) -> Result<bool, StorageError> {
        match self {
            Backend::Memory(s) => s.cas_ref(name, expected, new).await,
            Backend::Sqlite(s) => s.cas_ref(name, expected, new).await,
        }
    }

    async fn list_refs(&self, prefix: &str) -> Result<Vec<(String, Hash)>, StorageError> {
        match self {
            Backend::Memory(s) => s.list_refs(prefix).await,
            Backend::Sqlite(s) => s.list_refs(prefix).await,
        }
    }
}

/// Shared application state.
pub struct AppState {
    /// The engine.
    pub forked: Forked<Backend>,
    /// Optional static bearer token; `None` leaves the server open (local
    /// development).
    pub token: Option<String>,
}

/// Build the axum router over the given state.
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .fallback(handle)
        .layer(CorsLayer::permissive())
        .with_state(state)
}

async fn handle(State(state): State<Arc<AppState>>, req: Request) -> Response {
    if let Some(expected) = &state.token {
        let authorized = req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .is_some_and(|got| got == expected);
        if !authorized {
            return json_response(
                401,
                r#"{"error":{"code":"unauthorized","message":"missing or invalid bearer token"}}"#
                    .to_string(),
            );
        }
    }

    let (parts, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, BODY_LIMIT).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return json_response(
                413,
                r#"{"error":{"code":"payload_too_large","message":"request body too large"}}"#
                    .to_string(),
            );
        }
    };

    let api_req = ApiRequest {
        method: parts.method.as_str(),
        path: parts.uri.path(),
        query: parts.uri.query().unwrap_or(""),
        body: &bytes,
    };
    let res =
        forked_api::dispatch(&state.forked, api_req, || uuid::Uuid::new_v4().to_string()).await;
    json_response(res.status, res.body)
}

fn json_response(status: u16, body: String) -> Response {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() =
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}
