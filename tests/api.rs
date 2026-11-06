//! Tests for the API layer the desktop app calls. The routes return the status
//! and JSON the interface expects; there is no network involved.

use std::fs;
use agenticvisor::api::{call, Ctx};
use agenticvisor::search::index::new_shared;
use serde_json::{json, Value};

fn ctx_for(name: &str) -> Ctx {
    let home = std::env::temp_dir().join(format!("agv-api-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(&home).expect("create temp home");
    Ctx { home: home.clone(), data_dir: home.join("data"), index: new_shared() }
}

#[test]
fn known_route_answers_200_with_json() {
    let ctx = ctx_for("features");
    let answer = call(&ctx, "GET", "/api/features", &Value::Null);
    assert_eq!(answer.status, 200);
    assert_eq!(answer.body["preview"], json!(true));
}

#[test]
fn unknown_api_route_answers_404() {
    let ctx = ctx_for("unknown");
    let answer = call(&ctx, "GET", "/api/does-not-exist", &Value::Null);
    assert_eq!(answer.status, 404);
    assert_eq!(answer.body["ok"], json!(false));
}

#[test]
fn query_string_is_passed_to_the_route() {
    let ctx = ctx_for("query");
    let answer = call(&ctx, "GET", "/api/children?path=/definitely/not/here", &Value::Null);
    assert_eq!(answer.status, 200);
}

#[test]
fn unsupported_method_answers_405() {
    let ctx = ctx_for("method");
    let answer = call(&ctx, "DELETE", "/api/features", &Value::Null);
    assert_eq!(answer.status, 405);
}

#[test]
fn empty_trash_on_a_fresh_home_answers_ok() {
    let ctx = ctx_for("trash");
    let answer = call(&ctx, "POST", "/api/empty_trash", &json!({}));
    assert_eq!(answer.status, 200);
    assert_eq!(answer.body["ok"], json!(true));
}
