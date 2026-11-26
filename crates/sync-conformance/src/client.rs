//! A deliberately plain HTTP client: the suite should exercise the server
//! exactly as the protocol describes it, with no engine logic in between.

use serde_json::{json, Value};
use sync_core::http::percent_encode;
use sync_core::protocol::{HEADER_DEVICE, HEADER_SCHEMA};

pub struct Resp {
    pub status: u16,
    pub body: Value,
}

impl Resp {
    pub fn error_code(&self) -> Option<&str> {
        self.body.pointer("/error/code").and_then(Value::as_str)
    }
}

#[derive(Clone)]
pub struct Api {
    pub base_url: String,
    pub schema: i64,
    pub device: String,
}

pub enum Body<'a> {
    None,
    Json(&'a Value),
    Raw(&'a [u8]),
}

impl Api {
    pub fn new(base_url: &str, schema: i64) -> Api {
        Api {
            base_url: base_url.trim_end_matches('/').to_string(),
            schema,
            device: sync_core::new_id(),
        }
    }

    /// Same server, a different device ID.
    pub fn device(&self) -> Api {
        Api {
            device: sync_core::new_id(),
            ..self.clone()
        }
    }

    pub fn request(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        schema: i64,
        body: Body,
    ) -> Result<Resp, String> {
        let url = format!("{}{}", self.base_url, path);
        let mut req = ureq::request(method, &url)
            .set(HEADER_SCHEMA, &schema.to_string())
            .set(HEADER_DEVICE, &self.device)
            .timeout(std::time::Duration::from_secs(30));
        if let Some(t) = token {
            req = req.set("Authorization", &format!("Bearer {t}"));
        }
        let result = match body {
            Body::None => req.call(),
            Body::Json(v) => req
                .set("Content-Type", "application/json")
                .send_string(&v.to_string()),
            Body::Raw(b) => req.set("Content-Type", "application/json").send_bytes(b),
        };
        let resp = match result {
            Ok(r) => r,
            Err(ureq::Error::Status(_, r)) => r,
            Err(e) => return Err(format!("{method} {path}: {e}")),
        };
        let status = resp.status();
        let text = resp
            .into_string()
            .map_err(|e| format!("{method} {path}: reading body: {e}"))?;
        let body = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text)
                .map_err(|e| format!("{method} {path}: response is not JSON ({e}): {text}"))?
        };
        Ok(Resp { status, body })
    }

    pub fn session(&self, token: Option<&str>) -> Result<Resp, String> {
        self.request("GET", "/session", token, self.schema, Body::None)
    }

    pub fn push_raw(&self, token: &str, scope: &str, body: &Value) -> Result<Resp, String> {
        self.request(
            "POST",
            &changes_path(scope),
            Some(token),
            self.schema,
            Body::Json(body),
        )
    }

    /// Push changes and return the `results` array, requiring HTTP 200.
    pub fn push(
        &self,
        token: &str,
        scope: &str,
        changes: Vec<Value>,
    ) -> Result<Vec<Value>, String> {
        let n = changes.len();
        let r = self.push_raw(token, scope, &json!({ "changes": changes }))?;
        if r.status != 200 {
            return Err(format!("push: expected 200, got {} {}", r.status, r.body));
        }
        let results = r.body["results"]
            .as_array()
            .cloned()
            .ok_or("push: no results array")?;
        if results.len() != n {
            return Err(format!(
                "push: sent {n} changes, got {} results",
                results.len()
            ));
        }
        Ok(results)
    }

    pub fn push_one(&self, token: &str, scope: &str, change: Value) -> Result<Value, String> {
        Ok(self.push(token, scope, vec![change])?.remove(0))
    }

    pub fn pull_raw(
        &self,
        token: &str,
        scope: &str,
        cursor: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Resp, String> {
        let mut q = Vec::new();
        if let Some(c) = cursor {
            q.push(format!("cursor={}", percent_encode(c)));
        }
        if let Some(l) = limit {
            q.push(format!("limit={l}"));
        }
        let path = if q.is_empty() {
            changes_path(scope)
        } else {
            format!("{}?{}", changes_path(scope), q.join("&"))
        };
        self.request("GET", &path, Some(token), self.schema, Body::None)
    }

    /// One pull page, requiring HTTP 200.
    pub fn pull(
        &self,
        token: &str,
        scope: &str,
        cursor: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Page, String> {
        let r = self.pull_raw(token, scope, cursor, limit)?;
        if r.status != 200 {
            return Err(format!("pull: expected 200, got {} {}", r.status, r.body));
        }
        let records = r.body["records"]
            .as_array()
            .cloned()
            .ok_or("pull: no records array")?;
        let cursor = r.body["cursor"]
            .as_str()
            .ok_or("pull: no cursor")?
            .to_string();
        let has_more = r.body["has_more"].as_bool().ok_or("pull: no has_more")?;
        Ok(Page {
            records,
            cursor,
            has_more,
        })
    }

    /// Pull every page after `cursor`. Returns all records and the final
    /// cursor.
    pub fn pull_all(
        &self,
        token: &str,
        scope: &str,
        cursor: Option<&str>,
        limit: Option<usize>,
    ) -> Result<(Vec<Value>, String), String> {
        let mut cursor = cursor.map(str::to_string);
        let mut all = Vec::new();
        for _ in 0..100_000 {
            let page = self.pull(token, scope, cursor.as_deref(), limit)?;
            all.extend(page.records);
            cursor = Some(page.cursor);
            if !page.has_more {
                return Ok((all, cursor.unwrap()));
            }
        }
        Err("pull: has_more never became false".into())
    }
}

pub struct Page {
    pub records: Vec<Value>,
    pub cursor: String,
    pub has_more: bool,
}

pub fn changes_path(scope: &str) -> String {
    format!("/scopes/{}/changes", percent_encode(scope))
}

/// Raw POST to a test hook.
pub fn hook_post(test_url: &str, hook: &str, body: &Value) -> Result<Value, String> {
    let url = format!("{}/{}", test_url.trim_end_matches('/'), hook);
    match ureq::post(&url).send_string(&body.to_string()) {
        Ok(r) => r
            .into_string()
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string())),
        Err(e) => Err(format!("test hook {hook}: {e}")),
    }
}
