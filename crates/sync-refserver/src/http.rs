//! HTTP front end for [`RefServer`], on `tiny_http` with a few worker
//! threads.

use crate::RefServer;
use std::net::SocketAddr;
use std::sync::Arc;
use std::thread::JoinHandle;
use sync_core::http::{split_url, HttpRequest, Method};

pub struct ServerHandle {
    addr: SocketAddr,
    http: Arc<tiny_http::Server>,
    workers: Vec<JoinHandle<()>>,
}

impl ServerHandle {
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Base URL of the protocol, for example `http://127.0.0.1:8080/sync/v1`.
    pub fn base_url(&self) -> String {
        format!("http://{}/sync/v1", self.addr)
    }

    /// Base URL of the test hooks.
    pub fn test_url(&self) -> String {
        format!("http://{}/_test", self.addr)
    }

    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        for _ in &self.workers {
            self.http.unblock();
        }
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Serve `server` on `addr` (use port 0 for any free port) with `workers`
/// threads.
pub fn serve(server: Arc<RefServer>, addr: &str, workers: usize) -> std::io::Result<ServerHandle> {
    let http =
        Arc::new(tiny_http::Server::http(addr).map_err(|e| std::io::Error::other(e.to_string()))?);
    let addr = http
        .server_addr()
        .to_ip()
        .ok_or_else(|| std::io::Error::other("not an IP listener"))?;
    let workers = (0..workers.max(1))
        .map(|_| {
            let http = http.clone();
            let server = server.clone();
            std::thread::spawn(move || {
                while let Ok(rq) = http.recv() {
                    handle_one(&server, rq);
                }
            })
        })
        .collect();
    Ok(ServerHandle {
        addr,
        http,
        workers,
    })
}

fn handle_one(server: &RefServer, mut rq: tiny_http::Request) {
    let method = match rq.method() {
        tiny_http::Method::Get => Some(Method::Get),
        tiny_http::Method::Post => Some(Method::Post),
        _ => None,
    };
    let Some(method) = method else {
        let _ = rq.respond(tiny_http::Response::empty(405));
        return;
    };
    let (path, query) = split_url(rq.url());
    let headers = rq
        .headers()
        .iter()
        .map(|h| (h.field.as_str().to_string(), h.value.as_str().to_string()))
        .collect();
    let mut body = Vec::new();
    if rq.as_reader().read_to_end(&mut body).is_err() {
        let _ = rq.respond(tiny_http::Response::empty(400));
        return;
    }
    let req = HttpRequest {
        method,
        path,
        query,
        headers,
        body,
    };
    let resp = server.handle(&req);
    let mut out = tiny_http::Response::from_data(resp.body).with_status_code(resp.status);
    for (k, v) in resp.headers {
        if let Ok(h) = tiny_http::Header::from_bytes(k.as_bytes(), v.as_bytes()) {
            out.add_header(h);
        }
    }
    let _ = rq.respond(out);
}
