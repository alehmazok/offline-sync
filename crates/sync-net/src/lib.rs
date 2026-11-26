//! Transport and protocol client.
//!
//! A [`Transport`] moves one HTTP request; [`UreqTransport`] does it over
//! the network, and tests plug in an in-process server instead. [`Client`]
//! speaks protocol v1 on top of a transport: it adds the standard headers,
//! retries transient failures with backoff, refreshes the token once on 401
//! and maps error responses to [`ClientError`].

use serde::de::DeserializeOwned;
use std::sync::Arc;
use std::time::Duration;
use sync_core::http::{percent_encode, HttpRequest, HttpResponse, Method};
use sync_core::protocol::*;

// ------------------------------------------------------------------ transport

#[derive(Debug, Clone, thiserror::Error)]
#[error("network: {0}")]
pub struct TransportError(pub String);

pub trait Transport: Send + Sync {
    /// Send one request. `req.path` is relative to the protocol base URL.
    /// An `Err` means no usable response arrived (connection failure,
    /// timeout); HTTP error statuses are returned as `Ok`.
    fn send(&self, req: HttpRequest) -> Result<HttpResponse, TransportError>;
}

/// HTTPS transport on `ureq`.
pub struct UreqTransport {
    base_url: String,
    agent: ureq::Agent,
}

impl UreqTransport {
    pub fn new(base_url: &str) -> UreqTransport {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(15))
            .timeout(Duration::from_secs(60))
            .build();
        UreqTransport {
            base_url: base_url.trim_end_matches('/').to_string(),
            agent,
        }
    }
}

impl Transport for UreqTransport {
    fn send(&self, req: HttpRequest) -> Result<HttpResponse, TransportError> {
        let path: String = req
            .path
            .split('/')
            .map(percent_encode)
            .collect::<Vec<_>>()
            .join("/");
        let mut url = format!("{}{}", self.base_url, path);
        if !req.query.is_empty() {
            let q: Vec<String> = req
                .query
                .iter()
                .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
                .collect();
            url.push('?');
            url.push_str(&q.join("&"));
        }
        let mut r = self.agent.request(req.method.as_str(), &url);
        for (k, v) in &req.headers {
            r = r.set(k, v);
        }
        let result = match req.method {
            Method::Get => r.call(),
            Method::Post => r.send_bytes(&req.body),
        };
        let resp = match result {
            Ok(resp) => resp,
            Err(ureq::Error::Status(_, resp)) => resp,
            Err(e) => return Err(TransportError(e.to_string())),
        };
        let status = resp.status();
        let headers = resp
            .headers_names()
            .into_iter()
            .filter_map(|n| resp.header(&n).map(|v| (n.clone(), v.to_string())))
            .collect();
        let mut body = Vec::new();
        use std::io::Read;
        resp.into_reader()
            .take(64 * 1024 * 1024)
            .read_to_end(&mut body)
            .map_err(|e| TransportError(format!("reading response: {e}")))?;
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}

// ------------------------------------------------------------------ tokens

/// Supplies the bearer token. Called before every request; with
/// `force_refresh` after the server answered 401.
pub trait TokenProvider: Send + Sync {
    fn token(&self, force_refresh: bool) -> Result<String, String>;
}

/// A fixed token, for tests and simple setups.
pub struct StaticToken(pub String);

impl TokenProvider for StaticToken {
    fn token(&self, _force_refresh: bool) -> Result<String, String> {
        Ok(self.0.clone())
    }
}

// ------------------------------------------------------------------ errors

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ClientError {
    /// No response after all retries.
    #[error("network: {0}")]
    Network(String),
    /// 5xx or 429 after all retries, or an unexpected status.
    #[error("server error {status}: {code} {message}")]
    Server {
        status: u16,
        code: String,
        message: String,
    },
    #[error("unauthorized")]
    Unauthorized,
    #[error("token provider: {0}")]
    Token(String),
    /// 403 or 404 for a scope.
    #[error("no access to scope")]
    ScopeForbidden,
    #[error("server schema is older than the app's")]
    SchemaAhead,
    #[error("cursor expired")]
    CursorExpired,
    #[error("request too large")]
    TooLarge,
    /// With the oldest schema version the server still serves, if it said.
    #[error("app update required")]
    UpgradeRequired(Option<i64>),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("unreadable response: {0}")]
    Decode(String),
}

impl ClientError {
    /// Worth trying again in a later sync cycle.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            ClientError::Network(_) | ClientError::Server { .. } | ClientError::SchemaAhead
        )
    }
}

// ------------------------------------------------------------------ retry

#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Total attempts per request, including the first.
    pub attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            attempts: 4,
            base_delay: Duration::from_millis(300),
            max_delay: Duration::from_secs(8),
        }
    }
}

impl RetryPolicy {
    /// No waiting between attempts. For tests.
    pub fn immediate(attempts: u32) -> RetryPolicy {
        RetryPolicy {
            attempts,
            base_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
        }
    }

    fn delay(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        if let Some(d) = retry_after {
            return d.min(Duration::from_secs(60));
        }
        if self.base_delay.is_zero() {
            return Duration::ZERO;
        }
        let exp = self
            .base_delay
            .saturating_mul(1 << attempt.min(16))
            .min(self.max_delay);
        // Full jitter: a random point between zero and the exponential delay.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64)
            .unwrap_or(0);
        let frac = (nanos % 1000) as f64 / 1000.0;
        exp.mul_f64(0.5 + frac / 2.0)
    }
}

// ------------------------------------------------------------------ client

pub struct Client {
    transport: Arc<dyn Transport>,
    tokens: Arc<dyn TokenProvider>,
    schema: i64,
    device: String,
    retry: RetryPolicy,
}

impl Client {
    pub fn new(
        transport: Arc<dyn Transport>,
        tokens: Arc<dyn TokenProvider>,
        schema: i64,
        device: String,
        retry: RetryPolicy,
    ) -> Client {
        Client {
            transport,
            tokens,
            schema,
            device,
            retry,
        }
    }

    pub fn session(&self) -> Result<Session, ClientError> {
        self.call(Method::Get, "/session".into(), vec![], Vec::new())
    }

    /// Push a batch. `body` is the serialised [`PushRequest`]; it is passed
    /// as bytes so a resend after a crash is byte-identical.
    pub fn push(&self, scope: &str, body: &str) -> Result<PushResponse, ClientError> {
        self.call(
            Method::Post,
            changes_path(scope),
            vec![],
            body.as_bytes().to_vec(),
        )
    }

    pub fn pull(
        &self,
        scope: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<PullResponse, ClientError> {
        let mut q = vec![("limit".to_string(), limit.to_string())];
        if let Some(c) = cursor {
            q.push(("cursor".to_string(), c.to_string()));
        }
        self.call(Method::Get, changes_path(scope), q, Vec::new())
    }

    fn call<T: DeserializeOwned>(
        &self,
        method: Method,
        path: String,
        query: Vec<(String, String)>,
        body: Vec<u8>,
    ) -> Result<T, ClientError> {
        let mut refreshed = false;
        let mut attempt = 0;
        loop {
            let token = self.tokens.token(refreshed).map_err(ClientError::Token)?;
            let mut headers = vec![
                ("Authorization".to_string(), format!("Bearer {token}")),
                (HEADER_SCHEMA.to_string(), self.schema.to_string()),
                (HEADER_DEVICE.to_string(), self.device.clone()),
            ];
            if method == Method::Post {
                headers.push(("Content-Type".to_string(), "application/json".to_string()));
            }
            let req = HttpRequest {
                method: method.clone(),
                path: path.clone(),
                query: query.clone(),
                headers,
                body: body.clone(),
            };
            let outcome = self.transport.send(req);
            attempt += 1;
            let (retryable, err, retry_after) = match outcome {
                Err(e) => (true, ClientError::Network(e.0), None),
                Ok(resp) if (200..300).contains(&resp.status) => {
                    return serde_json::from_slice(&resp.body)
                        .map_err(|e| ClientError::Decode(e.to_string()));
                }
                Ok(resp) => {
                    let detail: Option<ErrorBody> = serde_json::from_slice(&resp.body).ok();
                    let min_schema = detail.as_ref().and_then(|d| d.error.min_schema_version);
                    let code = detail
                        .as_ref()
                        .map(|d| d.error.code.clone())
                        .unwrap_or_default();
                    let message = detail.and_then(|d| d.error.message).unwrap_or_default();
                    match resp.status {
                        401 if !refreshed => {
                            refreshed = true;
                            continue;
                        }
                        401 => return Err(ClientError::Unauthorized),
                        403 | 404 => return Err(ClientError::ScopeForbidden),
                        409 => return Err(ClientError::SchemaAhead),
                        410 => return Err(ClientError::CursorExpired),
                        413 => return Err(ClientError::TooLarge),
                        426 => return Err(ClientError::UpgradeRequired(min_schema)),
                        400 => return Err(ClientError::BadRequest(format!("{code} {message}"))),
                        429 | 500..=599 => {
                            let retry_after = resp
                                .header("Retry-After")
                                .and_then(|v| v.trim().parse::<u64>().ok())
                                .map(Duration::from_secs);
                            (
                                true,
                                ClientError::Server {
                                    status: resp.status,
                                    code,
                                    message,
                                },
                                retry_after,
                            )
                        }
                        s => {
                            return Err(ClientError::Server {
                                status: s,
                                code,
                                message,
                            })
                        }
                    }
                }
            };
            if !retryable || attempt >= self.retry.attempts {
                return Err(err);
            }
            let d = self.retry.delay(attempt, retry_after);
            log::debug!("{} {path}: {err}; retrying in {d:?}", method.as_str());
            if !d.is_zero() {
                std::thread::sleep(d);
            }
        }
    }
}

fn changes_path(scope: &str) -> String {
    format!("/scopes/{scope}/changes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Scripted {
        responses: Mutex<Vec<Result<HttpResponse, TransportError>>>,
        seen: Mutex<Vec<HttpRequest>>,
    }

    impl Transport for Scripted {
        fn send(&self, req: HttpRequest) -> Result<HttpResponse, TransportError> {
            self.seen.lock().unwrap().push(req);
            self.responses.lock().unwrap().remove(0)
        }
    }

    fn resp(status: u16, body: serde_json::Value) -> Result<HttpResponse, TransportError> {
        Ok(HttpResponse::json(status, &body))
    }

    struct Tokens(Mutex<Vec<bool>>);
    impl TokenProvider for Tokens {
        fn token(&self, force: bool) -> Result<String, String> {
            self.0.lock().unwrap().push(force);
            Ok(if force {
                "fresh".into()
            } else {
                "stale".into()
            })
        }
    }

    fn client(
        responses: Vec<Result<HttpResponse, TransportError>>,
    ) -> (Client, Arc<Scripted>, Arc<Tokens>) {
        let t = Arc::new(Scripted {
            responses: Mutex::new(responses),
            seen: Mutex::new(vec![]),
        });
        let tokens = Arc::new(Tokens(Mutex::new(vec![])));
        let c = Client::new(
            t.clone(),
            tokens.clone(),
            3,
            "dev".into(),
            RetryPolicy::immediate(3),
        );
        (c, t, tokens)
    }

    fn pull_ok() -> serde_json::Value {
        serde_json::json!({"records": [], "cursor": "5", "has_more": false})
    }

    #[test]
    fn retries_transient_failures() {
        let (c, t, _) = client(vec![
            Err(TransportError("reset".into())),
            resp(503, serde_json::json!({"error": {"code": "x"}})),
            resp(200, pull_ok()),
        ]);
        let r = c.pull("s", Some("1"), 10).unwrap();
        assert_eq!(r.cursor, "5");
        let seen = t.seen.lock().unwrap();
        assert_eq!(seen.len(), 3);
        assert_eq!(seen[0].header("X-Sync-Schema"), Some("3"));
        assert_eq!(seen[0].header("X-Sync-Device"), Some("dev"));
        assert_eq!(seen[0].query_param("cursor"), Some("1"));
    }

    #[test]
    fn gives_up_after_the_last_attempt() {
        let (c, _, _) = client(vec![
            Err(TransportError("a".into())),
            Err(TransportError("b".into())),
            Err(TransportError("c".into())),
        ]);
        assert_eq!(c.session(), Err(ClientError::Network("c".into())));
    }

    #[test]
    fn refreshes_the_token_once_on_401() {
        let (c, t, tokens) = client(vec![resp(401, serde_json::json!({})), resp(200, pull_ok())]);
        c.pull("s", None, 10).unwrap();
        assert_eq!(*tokens.0.lock().unwrap(), vec![false, true]);
        assert_eq!(
            t.seen.lock().unwrap()[1].header("Authorization"),
            Some("Bearer fresh")
        );

        let (c, _, _) = client(vec![
            resp(401, serde_json::json!({})),
            resp(401, serde_json::json!({})),
        ]);
        assert_eq!(c.session().unwrap_err(), ClientError::Unauthorized);
    }

    #[test]
    fn maps_error_statuses() {
        for (status, expected) in [
            (403, ClientError::ScopeForbidden),
            (404, ClientError::ScopeForbidden),
            (409, ClientError::SchemaAhead),
            (410, ClientError::CursorExpired),
            (413, ClientError::TooLarge),
            (426, ClientError::UpgradeRequired(None)),
        ] {
            let (c, t, _) = client(vec![resp(
                status,
                serde_json::json!({"error": {"code": "x"}}),
            )]);
            assert_eq!(c.session().unwrap_err(), expected);
            assert_eq!(
                t.seen.lock().unwrap().len(),
                1,
                "{status} must not be retried"
            );
        }
    }
}
