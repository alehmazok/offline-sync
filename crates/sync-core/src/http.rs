//! Plain HTTP values, so transports, the engine and the reference server can
//! exchange requests without depending on an HTTP library.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

impl Method {
    pub fn as_str(&self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
        }
    }
}

#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub method: Method,
    /// Path relative to the protocol base, for example `/session` or
    /// `/scopes/user:1/changes`. Already percent-decoded.
    pub path: String,
    /// Query parameters, decoded.
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        find_header(&self.headers, name)
    }

    pub fn query_param(&self, name: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn json(status: u16, body: &impl serde::Serialize) -> Self {
        HttpResponse {
            status,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: serde_json::to_vec(body).expect("serialisable body"),
        }
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        find_header(&self.headers, name)
    }
}

fn find_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// Percent-encode a path segment or query value. Unreserved characters
/// (RFC 3986) and `:` are kept.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b':') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

/// Decode percent-encoding and `+` as space. Invalid sequences are kept as
/// they are.
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(v) => {
                        out.push(v);
                        i += 3;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Split `path?query` into a decoded path and decoded query pairs.
pub fn split_url(url: &str) -> (String, Vec<(String, String)>) {
    let (path, query) = match url.split_once('?') {
        Some((p, q)) => (p, q),
        None => (url, ""),
    };
    let pairs = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (percent_decode(k), percent_decode(v)),
            None => (percent_decode(p), String::new()),
        })
        .collect();
    (percent_decode(path), pairs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let s = "user:1/ä b";
        assert_eq!(percent_decode(&percent_encode(s)), s);
        let (p, q) = split_url("/scopes/user%3A1/changes?cursor=12&limit=5");
        assert_eq!(p, "/scopes/user:1/changes");
        assert_eq!(
            q,
            vec![("cursor".into(), "12".into()), ("limit".into(), "5".into())]
        );
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }
}
