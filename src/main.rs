//! Command line entry point. The desktop app is in src-tauri/; this binary
//! prints the scan and the token usage for scripts.
//!
//!   agenticvisor scan-json       prints the scan as JSON
//!   agenticvisor usage-json      prints the token usage as JSON
//!   agenticvisor --version       prints the version
//!   agenticvisor bridge          test tooling: one JSON request per stdin line

use std::io::{BufRead, Write};

use serde_json::{json, Value};

use agenticvisor::api::{self, Ctx};
use agenticvisor::scanner::scan_json;
use agenticvisor::search::index::new_shared;
use agenticvisor::usage;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("scan-json") => print_json(&scan_json(&api::home_dir())),
        Some("usage-json") => {
            let records = usage::collect(&api::home_dir());
            if args.iter().any(|a| a == "--per-session") {
                let rows: Vec<_> = records.iter().map(|r| json!({"id": r.id, "by_model": r.usage.by_model})).collect();
                print_json(&json!(rows));
            } else {
                print_json(&usage::summary(&records));
            }
        }
        Some("bridge") => bridge(),
        Some("--version") | Some("-V") => println!("agenticvisor {}", env!("CARGO_PKG_VERSION")),
        None | Some("--help") | Some("-h") => {
            println!("agenticvisor scan-json | usage-json | bridge | --version");
        }
        Some(other) => {
            eprintln!("Unknown command: {other}");
            std::process::exit(2);
        }
    }
}

fn print_json(value: &Value) {
    println!("{}", serde_json::to_string_pretty(value).unwrap_or_default());
}

/// Test tooling: reads one request per stdin line, {"id", "method", "target", "body"},
/// and writes one answer per line, {"id", "status", "body"}. Nothing is listening on a port.
fn bridge() {
    let ctx = Ctx { home: api::home_dir(), data_dir: api::default_data_dir(), index: new_shared() };
    api::start_initial_index(&ctx);
    let stdout = std::io::stdout();
    for line in std::io::stdin().lock().lines().map_while(Result::ok) {
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let answer = api::call(
            &ctx,
            request["method"].as_str().unwrap_or("GET"),
            request["target"].as_str().unwrap_or("/"),
            &request["body"],
        );
        let mut out = stdout.lock();
        let _ = writeln!(out, "{}", json!({"id": request["id"], "status": answer.status, "body": answer.body}));
        let _ = out.flush();
    }
}
