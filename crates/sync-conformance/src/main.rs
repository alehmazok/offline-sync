//! Command-line runner. Exits with status 1 if any test fails.

use sync_conformance::{run, Config};

const USAGE: &str = "sync-conformance --base-url URL --token-a TOKEN --token-b TOKEN \
--scope-1 SCOPE --scope-2 SCOPE [--schema N] [--test-url URL] [--runs N] [--only PREFIX]";

fn main() {
    let mut cfg = Config {
        base_url: String::new(),
        token_a: String::new(),
        token_b: String::new(),
        scope_1: String::new(),
        scope_2: String::new(),
        schema: 1,
        test_url: None,
        concurrency_runs: 20,
    };
    let mut only = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = || {
            args.next()
                .unwrap_or_else(|| fail(&format!("{a} needs a value")))
        };
        match a.as_str() {
            "--base-url" => cfg.base_url = value(),
            "--token-a" => cfg.token_a = value(),
            "--token-b" => cfg.token_b = value(),
            "--scope-1" => cfg.scope_1 = value(),
            "--scope-2" => cfg.scope_2 = value(),
            "--schema" => {
                cfg.schema = value()
                    .parse()
                    .unwrap_or_else(|_| fail("--schema needs a number"))
            }
            "--test-url" => cfg.test_url = Some(value()),
            "--runs" => {
                cfg.concurrency_runs = value()
                    .parse()
                    .unwrap_or_else(|_| fail("--runs needs a number"))
            }
            "--only" => only = Some(value()),
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            other => fail(&format!("unknown argument {other}")),
        }
    }
    for (name, v) in [
        ("--base-url", &cfg.base_url),
        ("--token-a", &cfg.token_a),
        ("--token-b", &cfg.token_b),
        ("--scope-1", &cfg.scope_1),
        ("--scope-2", &cfg.scope_2),
    ] {
        if v.is_empty() {
            fail(&format!("{name} is required"));
        }
    }

    let outcomes = run(&cfg, only.as_deref(), |o| match o.passed {
        Some(true) => println!("PASS  {}  {}", o.id, o.title),
        Some(false) => println!("FAIL  {}  {}\n      {}", o.id, o.title, o.detail),
        None => println!("SKIP  {}  {} ({})", o.id, o.title, o.detail),
    });
    let failed = outcomes.iter().filter(|o| o.passed == Some(false)).count();
    let passed = outcomes.iter().filter(|o| o.passed == Some(true)).count();
    let skipped = outcomes.iter().filter(|o| o.passed.is_none()).count();
    println!("\n{passed} passed, {failed} failed, {skipped} skipped");
    if failed > 0 {
        std::process::exit(1);
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("sync-conformance: {msg}\nusage: {USAGE}");
    std::process::exit(2);
}
