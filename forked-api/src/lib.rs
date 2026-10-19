//! # forked-api
//!
//! The HTTP API contract for forked, shared by the self-hosted server
//! (`forked-server`) and forked cloud (`forked-cloudflare`):
//!
//! - [`dto`]: request/response JSON types (mirrored by the TypeScript SDK),
//! - [`error`]: the standard error body and status mapping,
//! - [`handlers`]: transport-agnostic typed handlers over a `Forked` engine,
//! - [`dispatch`]: a full `(method, path, query, body)` router, so every
//!   deployment exposes identical routes by construction.
//!
//! Compiles for `wasm32-unknown-unknown`; no HTTP framework dependencies.

#![forbid(unsafe_code)]

pub mod dto;
pub mod error;
pub mod handlers;

mod dispatch;

pub use dispatch::{dispatch, ApiRequest, ApiResponse};
pub use error::{ApiError, ApiResult, ErrorBody};
