//! HTTP JSON-RPC client for signal-cli daemon mode.

use {
    reqwest::{Client, Response},
    serde::{Deserialize, Serialize, de::DeserializeOwned},
    serde_json::{Value, json},
    std::{error::Error as StdError, time::Duration},
    uuid::Uuid,
};

#[derive(Clone, Debug)]
pub struct SignalClient {
    /// Short-lived RPC requests (connect 10s, overall 30s).
    http: Client,
    /// Long-lived SSE stream (connect 10s, no overall timeout).
    sse: Client,
}

#[derive(Debug, Deserialize)]
struct RpcResponse<T> {
    result: Option<T>,
    error: Option<RpcError>,
}

#[derive(Debug, Deserialize)]
struct RpcError {
    code: Option<i64>,
    message: Option<String>,
}

#[derive(Serialize)]
struct RpcRequest<'a> {
    jsonrpc: &'static str,
    method: &'a str,
    params: Value,
    id: String,
}

impl Default for SignalClient {
    fn default() -> Self {
        Self::new()
    }
}

impl SignalClient {
    pub fn new() -> Self {
        Self {
            http: Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            sse: Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
        }
    }

    pub async fn check(&self, base_url: &str) -> moltis_channels::Result<Value> {
        let url = format!("{}/api/v1/check", base_url.trim_end_matches('/'));
        let res = self.http.get(&url).send().await.map_err(|e| {
            unavailable_with_source(
                format!("could not reach signal-cli daemon at {url}; is the daemon running?"),
                &e,
            )
        })?;
        let status = res.status();
        if !status.is_success() {
            return Err(moltis_channels::Error::unavailable(format!(
                "signal-cli check failed with HTTP {status}"
            )));
        }
        let text = res
            .text()
            .await
            .map_err(|e| moltis_channels::Error::external("signal-cli check body", e))?;
        parse_check_body(&text)
    }

    pub async fn stream_events(
        &self,
        base_url: &str,
        account: Option<&str>,
    ) -> moltis_channels::Result<Response> {
        let mut url = url::Url::parse(&format!("{}/api/v1/events", base_url.trim_end_matches('/')))
            .map_err(|e| {
                moltis_channels::Error::invalid_input(format!("invalid Signal daemon URL: {e}"))
            })?;
        if let Some(account) = account {
            url.query_pairs_mut().append_pair("account", account);
        }
        self.sse
            .get(url.clone())
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .send()
            .await
            .map_err(|e| {
                unavailable_with_source(
                    format!("could not reach signal-cli daemon at {url}; is the daemon running?"),
                    &e,
                )
            })
    }

    pub async fn rpc_value(
        &self,
        base_url: &str,
        method: &str,
        params: Value,
    ) -> moltis_channels::Result<Value> {
        self.rpc(base_url, method, params).await
    }

    pub async fn rpc<T>(
        &self,
        base_url: &str,
        method: &str,
        params: Value,
    ) -> moltis_channels::Result<T>
    where
        T: DeserializeOwned,
    {
        let url = format!("{}/api/v1/rpc", base_url.trim_end_matches('/'));
        let body = RpcRequest {
            jsonrpc: "2.0",
            method,
            params,
            id: Uuid::new_v4().to_string(),
        };

        let res = self.http.post(&url).json(&body).send().await.map_err(|e| {
            unavailable_with_source(
                format!("could not reach signal-cli daemon at {url}; is the daemon running?"),
                &e,
            )
        })?;
        let status = res.status();
        if !status.is_success() {
            return Err(moltis_channels::Error::unavailable(format!(
                "signal-cli RPC {method} failed with HTTP {status}"
            )));
        }

        let text = res
            .text()
            .await
            .map_err(|e| moltis_channels::Error::external("signal-cli rpc body", e))?;
        if text.trim().is_empty() {
            return serde_json::from_value(json!({}))
                .map_err(|e| moltis_channels::Error::external("signal-cli empty rpc response", e));
        }

        let parsed: RpcResponse<T> = serde_json::from_str(&text).map_err(|e| {
            moltis_channels::Error::external("signal-cli malformed rpc response", e)
        })?;
        if let Some(error) = parsed.error {
            let code = error
                .code
                .map_or_else(|| "unknown".to_string(), |v| v.to_string());
            let message = error
                .message
                .unwrap_or_else(|| "Signal RPC error".to_string());
            return Err(moltis_channels::Error::unavailable(format!(
                "signal-cli RPC {method} error {code}: {message}"
            )));
        }
        parsed.result.ok_or_else(|| {
            moltis_channels::Error::unavailable(format!(
                "signal-cli RPC {method} returned no result"
            ))
        })
    }
}

/// Parse the `/api/v1/check` response body.
///
/// The daemon returns `200 OK` with an empty body when healthy, so empty
/// (or whitespace-only) bodies map to an empty JSON object instead of a
/// JSON parse error.
fn parse_check_body(text: &str) -> moltis_channels::Result<Value> {
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(text)
        .map_err(|e| moltis_channels::Error::external("signal-cli check json", e))
}

/// Build a retryable `Unavailable` error that preserves the full source
/// chain.
///
/// `reqwest::Error`'s `Display` only shows the top-level message (e.g.
/// "error sending request for url (...)"), hiding the root cause such
/// as "connection refused". Walking `source()` keeps that detail in
/// the message that reaches the logs and the UI.
fn unavailable_with_source(prefix: String, err: &dyn StdError) -> moltis_channels::Error {
    let mut parts = vec![err.to_string()];
    let mut source = err.source();
    while let Some(next) = source {
        parts.push(next.to_string());
        source = next.source();
    }
    moltis_channels::Error::unavailable(format!("{}: {}", prefix, parts.join(": ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_body_tolerates_empty_success_response() {
        assert_eq!(parse_check_body("").unwrap(), json!({}));
        assert_eq!(parse_check_body("  \n\t ").unwrap(), json!({}));
    }

    #[test]
    fn check_body_parses_json_when_present() {
        let value = parse_check_body(r#"{"ok":true}"#).unwrap();
        assert_eq!(value, json!({"ok": true}));
    }

    #[test]
    fn check_body_rejects_malformed_json() {
        assert!(parse_check_body("not json").is_err());
    }

    #[test]
    fn unavailable_error_keeps_root_cause() {
        let io_err =
            std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "connection refused");
        let err = unavailable_with_source("could not reach signal-cli daemon at http://127.0.0.1:8478/api/v1/check; is the daemon running?".to_string(), &io_err);
        let message = err.to_string();
        assert!(message.contains("http://127.0.0.1:8478"), "{message}");
        assert!(message.contains("connection refused"), "{message}");
        assert!(err.is_retryable());
    }
}
