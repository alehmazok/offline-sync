//! `sync-refserver [--config FILE] [--listen ADDR] [--db FILE] [--workers N]`
//!
//! Without `--config` it serves the conformance fixture (tokens `token-a`
//! and `token-b`, scopes `team:conf1` and `team:conf2`, test hooks on).

use std::sync::Arc;
use sync_refserver::{serve, RefServer, ServerConfig};

fn main() {
    let mut config_path = None;
    let mut listen = "127.0.0.1:8080".to_string();
    let mut db = None;
    let mut workers = 4;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = || {
            args.next()
                .unwrap_or_else(|| fail(&format!("{a} needs a value")))
        };
        match a.as_str() {
            "--config" => config_path = Some(value()),
            "--listen" => listen = value(),
            "--db" => db = Some(value()),
            "--workers" => {
                workers = value()
                    .parse()
                    .unwrap_or_else(|_| fail("--workers needs a number"))
            }
            "-h" | "--help" => {
                println!(
                    "sync-refserver [--config FILE] [--listen ADDR] [--db FILE] [--workers N]"
                );
                return;
            }
            other => fail(&format!("unknown argument {other}")),
        }
    }
    let config = match config_path {
        Some(p) => {
            let text = std::fs::read_to_string(&p).unwrap_or_else(|e| fail(&format!("{p}: {e}")));
            serde_json::from_str(&text).unwrap_or_else(|e| fail(&format!("{p}: {e}")))
        }
        None => ServerConfig::conformance(),
    };
    let server = match db {
        Some(path) => RefServer::open(config, &path),
        None => RefServer::new(config),
    }
    .unwrap_or_else(|e| fail(&e.to_string()));
    let handle = serve(Arc::new(server), &listen, workers).unwrap_or_else(|e| fail(&e.to_string()));
    println!("sync-refserver listening on {}", handle.base_url());
    loop {
        std::thread::park();
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("sync-refserver: {msg}");
    std::process::exit(2);
}
