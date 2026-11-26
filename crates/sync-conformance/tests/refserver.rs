//! The reference server must always pass the whole suite. This is also how
//! the suite itself is tested.

use std::sync::Arc;
use sync_conformance::{run, Config};
use sync_refserver::{serve, RefServer, ServerConfig};

#[test]
fn reference_server_passes_the_suite() {
    let server = Arc::new(RefServer::new(ServerConfig::conformance()).unwrap());
    let handle = serve(server, "127.0.0.1:0", 8).unwrap();
    let cfg = Config {
        base_url: handle.base_url(),
        token_a: "token-a".into(),
        token_b: "token-b".into(),
        scope_1: "team:conf1".into(),
        scope_2: "team:conf2".into(),
        schema: 1,
        test_url: Some(handle.test_url()),
        concurrency_runs: 5,
    };
    let outcomes = run(&cfg, None, |o| {
        if o.passed == Some(false) {
            eprintln!("FAIL {} {}: {}", o.id, o.title, o.detail);
        }
    });
    let failed: Vec<_> = outcomes
        .iter()
        .filter(|o| o.passed != Some(true))
        .map(|o| o.id)
        .collect();
    assert!(failed.is_empty(), "failed or skipped: {failed:?}");
    assert_eq!(outcomes.len(), 44);
}
