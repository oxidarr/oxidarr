//! The real [`crate::api::Http`] implementation for the browser: a thin
//! wrapper over `gloo-net`'s `fetch`-backed `Request`/`Response` (verified
//! against `gloo-net` `0.7.0`'s own source,
//! `~/.cargo/registry/src/.../gloo-net-0.7.0/src/http/request.rs` — this
//! workspace has no wasm toolchain installed by default, so that source
//! read stood in for a real compile check; see this task's own report for
//! how far that was verified). `wasm32`-only: [`ApiClient`] is generic over
//! [`Http`] precisely so nothing else in this crate needs this module at
//! all — `crate::app`'s `NativeHttp` fills the same role on every other
//! target, and this crate's own tests use a scripted fake (`crate::api`'s
//! own `tests` module).

use gloo_net::http::Request;

use crate::api::{Http, UiError};

/// The real `Http` transport used by [`crate::app::App`] on `wasm32`.
#[derive(Debug, Default, Clone, Copy)]
pub struct WasmHttp;

impl Http for WasmHttp {
    async fn request(
        &self,
        method: &str,
        path: &str,
        key: Option<&str>,
        body: Option<String>,
    ) -> Result<(u16, String), UiError> {
        let mut builder = match method {
            "GET" => Request::get(path),
            "POST" => Request::post(path),
            "PUT" => Request::put(path),
            "DELETE" => Request::delete(path),
            other => {
                return Err(UiError::Network(format!(
                    "WasmHttp does not support method {other}"
                )));
            }
        };
        if let Some(key) = key {
            builder = builder.header("X-Api-Key", key);
        }

        let request = match body {
            Some(body) => builder
                .header("Content-Type", "application/json")
                .body(body),
            None => builder.build(),
        }
        .map_err(|err| UiError::Network(err.to_string()))?;

        let response = request
            .send()
            .await
            .map_err(|err| UiError::Network(err.to_string()))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|err| UiError::Network(err.to_string()))?;
        Ok((status, text))
    }
}
