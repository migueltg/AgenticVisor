//! The dashboard API: every route the interface calls, as plain functions.
//!
//! The desktop app calls `call` through a Tauri command. The routes, their
//! parameters and their response shapes are the ones the interface expects.
//! There is no network server: nothing listens on a port.

use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::{json, Map, Value};

use crate::search::index::{self, SharedIndex, MIN_QUERY_CHARS};
use crate::{artifacts, backups, browse, config, cursor_chats, extensions, preview, projects, runtime, scanner, trash, usage};

/// What the API needs to answer any request.
pub struct Ctx {
    pub home: PathBuf,
    pub data_dir: PathBuf,
    pub index: SharedIndex,
}

/// An answer: a status code in the HTTP style and a JSON body.
pub struct Reply {
    pub status: u16,
    pub body: Value,
}

impl Ctx {
    pub fn trash_dir(&self) -> PathBuf {
        self.data_dir.join("trash")
    }
    pub fn backups_dir(&self) -> PathBuf {
        self.data_dir.join("backups")
    }
    pub fn index_db(&self) -> PathBuf {
        self.data_dir.join("search_index.sqlite")
    }
    pub fn index_config(&self) -> PathBuf {
        self.data_dir.join("search_config.json")
    }
    fn index_info(&self) -> Value {
        index::info(&self.index, &self.index_db(), &self.index_config())
    }
}

/// Starts the background indexing pass that runs at launch.
pub fn start_initial_index(ctx: &Ctx) {
    projects::load_hints(&ctx.data_dir);
    index::start(&ctx.index, ctx.home.clone(), ctx.index_db(), ctx.index_config());
}

/// Answers one request. `target` is the path with its query string, as the interface builds it.
pub fn call(ctx: &Ctx, method: &str, target: &str, body: &Value) -> Reply {
    let (path, query) = split_target(target);
    match method {
        "GET" => get(ctx, &path, &query),
        "POST" => post(ctx, &path, body),
        _ => reply(405, json!({"ok": false, "error": "Method not allowed"})),
    }
}

fn reply(status: u16, body: Value) -> Reply {
    Reply { status, body }
}

fn ok(body: Value) -> Reply {
    reply(200, body)
}

fn not_found(path: &str) -> Reply {
    if path.starts_with("/api/") {
        reply(404, json!({"ok": false, "error": "Route not found"}))
    } else {
        reply(404, json!({"ok": false, "error": "Not found"}))
    }
}

fn split_target(target: &str) -> (String, HashMap<String, String>) {
    match target.split_once('?') {
        Some((path, query)) => (path.to_string(), parse_query(query)),
        None => (target.to_string(), HashMap::new()),
    }
}

fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter(|kv| !kv.is_empty())
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (decode(k), decode(v))
        })
        .collect()
}

/// Decodes a URL value like parse_qs: '+' is a space and %XX is a byte.
fn decode(s: &str) -> String {
    let replaced = s.replace('+', " ");
    percent_encoding::percent_decode_str(&replaced).decode_utf8_lossy().into_owned()
}

/// Response of the actions: {ok: true, ...} or {ok: false, error}.
fn action(result: Result<Value, String>) -> Value {
    match result {
        Ok(Value::Object(mut map)) => {
            let mut out = Map::new();
            out.insert("ok".into(), json!(true));
            out.append(&mut map);
            Value::Object(out)
        }
        Ok(other) => json!({"ok": true, "value": other}),
        Err(error) => json!({"ok": false, "error": error}),
    }
}

fn str_field<'a>(body: &'a Value, name: &str) -> &'a str {
    body.get(name).and_then(Value::as_str).unwrap_or("")
}

fn u64_field(body: &Value, name: &str) -> u64 {
    body.get(name).and_then(Value::as_u64).unwrap_or(0)
}

fn post(ctx: &Ctx, path: &str, body: &Value) -> Reply {
    match path {
        "/api/trash" => {
            let mut results = Map::new();
            for p in body.get("paths").and_then(Value::as_array).into_iter().flatten() {
                if let Some(p) = p.as_str() {
                    // A Claude Code session is deleted with everything that belongs to it.
                    let result = if projects::claude_session(&ctx.home, p).is_ok() {
                        action(projects::delete_claude_session(&ctx.home, &ctx.trash_dir(), &ctx.backups_dir(), p))
                    } else {
                        trash::move_to_trash(&ctx.home, &ctx.trash_dir(), p)
                    };
                    results.insert(p.to_string(), result);
                }
            }
            ok(json!({"results": results}))
        }
        "/api/empty_trash" => ok(trash::empty_trash(&ctx.trash_dir())),
        "/api/session_move" => {
            let result = projects::move_claude_session(&ctx.home, &ctx.backups_dir(), str_field(body, "path"), str_field(body, "target"));
            if result.is_ok() {
                projects::save_hints(&ctx.data_dir);
            }
            ok(action(result))
        }
        "/api/project_delete" => {
            let running = runtime::is_running(&runtime::process_names(), "cursor-desktop");
            ok(action(projects::delete_project(&ctx.home, &ctx.trash_dir(), &ctx.backups_dir(), str_field(body, "project"), running)))
        }
        "/api/backups_delete" => {
            let names: Vec<String> = body
                .get("names")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|n| n.as_str().map(str::to_string))
                .collect();
            ok(backups::move_to_trash(&ctx.home, &ctx.backups_dir(), &ctx.trash_dir(), &names))
        }
        "/api/cursor_chat_delete" => {
            let running = runtime::is_running(&runtime::process_names(), "cursor-desktop");
            let response = action(cursor_chats::delete_chat(&ctx.home, &ctx.backups_dir(), str_field(body, "key"), running));
            // Rebuild the search index so the deleted conversation stops appearing in results.
            if response["ok"] == json!(true) {
                index::start(&ctx.index, ctx.home.clone(), ctx.index_db(), ctx.index_config());
            }
            ok(response)
        }
        "/api/reindex" => {
            let started = index::start(&ctx.index, ctx.home.clone(), ctx.index_db(), ctx.index_config());
            let mut info = ctx.index_info();
            info["started"] = json!(started);
            ok(info)
        }
        "/api/search_settings" => {
            let mode = str_field(body, "tool_outputs").to_string();
            let running = ctx.index.lock().map(|s| s.running).unwrap_or(false);
            let response = if !index::TOOL_OUTPUT_MODES.contains(&mode.as_str()) {
                json!({"ok": false, "error": format!("Invalid option: {mode}")})
            } else if running {
                json!({"ok": false, "error": "An indexing run is in progress: wait for it to finish and choose again."})
            } else {
                match index::write_tool_mode(&ctx.index_config(), &mode) {
                    Ok(()) => {
                        index::start(&ctx.index, ctx.home.clone(), ctx.index_db(), ctx.index_config());
                        let mut info = ctx.index_info();
                        info["ok"] = json!(true);
                        info
                    }
                    Err(error) => json!({"ok": false, "error": error}),
                }
            };
            ok(response)
        }
        "/api/plugin_toggle" => ok(action(extensions::set_plugin_enabled(
            &ctx.home,
            str_field(body, "id"),
            body.get("enabled").and_then(Value::as_bool).unwrap_or(false),
            &ctx.backups_dir(),
        ))),
        "/api/skill_toggle" => ok(action(extensions::set_skill_disabled(
            &ctx.home,
            str_field(body, "name"),
            body.get("disabled").and_then(Value::as_bool).unwrap_or(false),
        ))),
        "/api/skill_file" => ok(action(extensions::write_skill(
            &ctx.home,
            str_field(body, "path"),
            str_field(body, "content"),
            u64_field(body, "mtime_ns"),
            &ctx.backups_dir(),
        ))),
        "/api/config" => ok(action(config::write(
            &ctx.home,
            str_field(body, "id"),
            str_field(body, "content"),
            u64_field(body, "mtime_ns"),
            &ctx.backups_dir(),
        ))),
        _ => not_found(path),
    }
}

fn get(ctx: &Ctx, path: &str, query: &HashMap<String, String>) -> Reply {
    let q = |name: &str| query.get(name).map(String::as_str).unwrap_or("");
    match path {
        "/api/scan" => ok(scanner::scan_json(&ctx.home)),
        "/api/projects" => ok(projects::list(&scanner::run_scan(&ctx.home).entries)),
        "/api/children" => ok(browse::list_children(&ctx.home, q("path"))),
        "/api/trash_info" => ok(trash::trash_info(&ctx.trash_dir())),
        "/api/usage" => ok(usage::summary(&usage::collect(&ctx.home))),
        "/api/features" => ok(json!({
            "search": true, "skills": true, "plugins": true,
            "artifacts": true, "config": true, "preview": true,
        })),
        "/api/index_status" => ok(ctx.index_info()),
        "/api/content_search" => {
            let query_text = q("q").to_string();
            let results = if query_text.trim().chars().count() >= MIN_QUERY_CHARS {
                index::search(&ctx.index_db(), &query_text, 50).unwrap_or_default()
            } else {
                Vec::new()
            };
            ok(json!({
                "query": query_text,
                "min_chars": MIN_QUERY_CHARS,
                "results": results,
                "index": ctx.index_info(),
            }))
        }
        "/api/session_preview" => ok(action(preview::preview(&ctx.home, q("key"), q("offset"), preview::PAGE_SIZE))),
        "/api/extensions" => ok(extensions::inventory(&ctx.home)),
        "/api/skill_file" => ok(action(extensions::read_skill(&ctx.home, q("path")))),
        "/api/configs" => ok(json!({"items": config::list(&ctx.home)})),
        "/api/config" => ok(action(config::read(&ctx.home, q("id")))),
        "/api/artifacts" => ok(artifacts::inventory(&ctx.home)),
        "/api/runtime" => ok(runtime::status(&runtime::process_names())),
        "/api/backups" => ok(backups::list(&ctx.backups_dir())),
        "/api/canvas_file" => ok(action(artifacts::read_canvas(&ctx.home, q("path")))),
        _ => not_found(path),
    }
}

/// The user's home folder, or the current folder when it cannot be found.
pub fn home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// The dashboard's own data folder (trash, backups, search index). Can be changed
/// with AGENTICVISOR_DATA, for example in tests.
pub fn default_data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("AGENTICVISOR_DATA") {
        return PathBuf::from(dir);
    }
    dirs::data_local_dir()
        .unwrap_or_else(|| home_dir().join(".local").join("share"))
        .join("agenticvisor")
}
