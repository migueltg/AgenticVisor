//! Local server (127.0.0.1 only) with the interface embedded in the binary.
//! The routes and response shapes are the ones the interface expects,
//! so the interface works unchanged.

use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::{json, Map, Value};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::search::index::{self, SharedIndex, MIN_QUERY_CHARS};
use crate::{artifacts, backups, browse, config, cursor_chats, extensions, preview, runtime, scanner, trash, usage};

const DASHBOARD_HTML: &str = include_str!("../static/dashboard.html");
const CM_JSON: &[u8] = include_bytes!("../static/vendor/cm-json.js");

/// What the server needs to handle any request.
pub struct Ctx {
    pub home: PathBuf,
    pub data_dir: PathBuf,
    pub index: SharedIndex,
}

impl Ctx {
    fn trash_dir(&self) -> PathBuf {
        self.data_dir.join("trash")
    }
    fn backups_dir(&self) -> PathBuf {
        self.data_dir.join("backups")
    }
    fn index_db(&self) -> PathBuf {
        self.data_dir.join("search_index.sqlite")
    }
    fn index_config(&self) -> PathBuf {
        self.data_dir.join("search_config.json")
    }
    fn index_info(&self) -> Value {
        index::info(&self.index, &self.index_db(), &self.index_config())
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

fn send_json(request: Request, status: u16, body: &Value) {
    let data = serde_json::to_vec(body).unwrap_or_default();
    let header = Header::from_bytes("Content-Type", "application/json").expect("valid header");
    let _ = request.respond(Response::from_data(data).with_status_code(status).with_header(header));
}

fn send_bytes(request: Request, content_type: &str, body: &[u8]) {
    let header = Header::from_bytes("Content-Type", content_type).expect("valid header");
    let _ = request.respond(Response::from_data(body.to_vec()).with_header(header));
}

fn read_body(request: &mut Request) -> Value {
    let mut text = String::new();
    let _ = std::io::Read::read_to_string(request.as_reader(), &mut text);
    serde_json::from_str(&text).unwrap_or_else(|_| json!({}))
}

fn str_field<'a>(body: &'a Value, name: &str) -> &'a str {
    body.get(name).and_then(Value::as_str).unwrap_or("")
}

fn u64_field(body: &Value, name: &str) -> u64 {
    body.get(name).and_then(Value::as_u64).unwrap_or(0)
}

fn handle_post(mut request: Request, ctx: &Ctx, path: &str) {
    let body = read_body(&mut request);
    match path {
        "/api/trash" => {
            let mut results = Map::new();
            for p in body.get("paths").and_then(Value::as_array).into_iter().flatten() {
                if let Some(p) = p.as_str() {
                    results.insert(p.to_string(), trash::move_to_trash(&ctx.home, &ctx.trash_dir(), p));
                }
            }
            send_json(request, 200, &json!({"results": results}));
        }
        "/api/empty_trash" => send_json(request, 200, &trash::empty_trash(&ctx.trash_dir())),
        "/api/backups_delete" => {
            let names: Vec<String> = body
                .get("names")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|n| n.as_str().map(str::to_string))
                .collect();
            send_json(request, 200, &backups::move_to_trash(&ctx.home, &ctx.backups_dir(), &ctx.trash_dir(), &names));
        }
        "/api/cursor_chat_delete" => {
            let running = runtime::is_running(&runtime::process_names(), "cursor-desktop");
            let response = action(cursor_chats::delete_chat(&ctx.home, &ctx.backups_dir(), str_field(&body, "key"), running));
            // Rebuild the search index so the deleted conversation stops appearing in results.
            if response["ok"] == json!(true) {
                index::start(&ctx.index, ctx.home.clone(), ctx.index_db(), ctx.index_config());
            }
            send_json(request, 200, &response);
        }
        "/api/reindex" => {
            let started = index::start(&ctx.index, ctx.home.clone(), ctx.index_db(), ctx.index_config());
            let mut info = ctx.index_info();
            info["started"] = json!(started);
            send_json(request, 200, &info);
        }
        "/api/search_settings" => {
            let mode = str_field(&body, "tool_outputs").to_string();
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
            send_json(request, 200, &response);
        }
        "/api/plugin_toggle" => {
            let result = extensions::set_plugin_enabled(
                &ctx.home,
                str_field(&body, "id"),
                body.get("enabled").and_then(Value::as_bool).unwrap_or(false),
                &ctx.backups_dir(),
            );
            send_json(request, 200, &action(result));
        }
        "/api/skill_toggle" => {
            let result = extensions::set_skill_disabled(
                &ctx.home,
                str_field(&body, "name"),
                body.get("disabled").and_then(Value::as_bool).unwrap_or(false),
            );
            send_json(request, 200, &action(result));
        }
        "/api/skill_file" => {
            let result = extensions::write_skill(
                &ctx.home,
                str_field(&body, "path"),
                str_field(&body, "content"),
                u64_field(&body, "mtime_ns"),
                &ctx.backups_dir(),
            );
            send_json(request, 200, &action(result));
        }
        "/api/config" => {
            let result = config::write(
                &ctx.home,
                str_field(&body, "id"),
                str_field(&body, "content"),
                u64_field(&body, "mtime_ns"),
                &ctx.backups_dir(),
            );
            send_json(request, 200, &action(result));
        }
        _ => send_json(request, 404, &json!({"ok": false, "error": "Route not found"})),
    }
}

fn handle_get(request: Request, ctx: &Ctx, path: &str, query: &HashMap<String, String>) {
    let q = |name: &str| query.get(name).map(String::as_str).unwrap_or("");
    match path {
        "/" | "/index.html" => send_bytes(request, "text/html; charset=utf-8", DASHBOARD_HTML.as_bytes()),
        "/vendor/cm-json.js" => send_bytes(request, "application/javascript; charset=utf-8", CM_JSON),
        "/api/scan" => send_json(request, 200, &scanner::scan_json(&ctx.home)),
        "/api/children" => send_json(request, 200, &browse::list_children(&ctx.home, q("path"))),
        "/api/trash_info" => send_json(request, 200, &trash::trash_info(&ctx.trash_dir())),
        "/api/usage" => {
            let records = usage::collect(&ctx.home);
            send_json(request, 200, &usage::summary(&records));
        }
        "/api/features" => send_json(
            request,
            200,
            &json!({
                "search": true, "skills": true, "plugins": true,
                "artifacts": true, "config": true, "preview": true,
            }),
        ),
        "/api/index_status" => send_json(request, 200, &ctx.index_info()),
        "/api/content_search" => {
            let query_text = q("q").to_string();
            let results = if query_text.trim().chars().count() >= MIN_QUERY_CHARS {
                index::search(&ctx.index_db(), &query_text, 50).unwrap_or_default()
            } else {
                Vec::new()
            };
            send_json(
                request,
                200,
                &json!({
                    "query": query_text,
                    "min_chars": MIN_QUERY_CHARS,
                    "results": results,
                    "index": ctx.index_info(),
                }),
            );
        }
        "/api/session_preview" => {
            let result = preview::preview(&ctx.home, q("key"), q("offset"), preview::PAGE_SIZE);
            send_json(request, 200, &action(result));
        }
        "/api/extensions" => send_json(request, 200, &extensions::inventory(&ctx.home)),
        "/api/skill_file" => send_json(request, 200, &action(extensions::read_skill(&ctx.home, q("path")))),
        "/api/configs" => send_json(request, 200, &json!({"items": config::list(&ctx.home)})),
        "/api/config" => send_json(request, 200, &action(config::read(&ctx.home, q("id")))),
        "/api/artifacts" => send_json(request, 200, &artifacts::inventory(&ctx.home)),
        "/api/runtime" => send_json(request, 200, &runtime::status(&runtime::process_names())),
        "/api/backups" => send_json(request, 200, &backups::list(&ctx.backups_dir())),
        "/api/canvas_file" => send_json(request, 200, &action(artifacts::read_canvas(&ctx.home, q("path")))),
        p if p.starts_with("/api/") => send_json(request, 404, &json!({"ok": false, "error": "Route not found"})),
        _ => send_json(request, 404, &json!({"ok": false, "error": "Not found"})),
    }
}

/// Starts the server. Blocks until the process stops.
pub fn serve(addr: &str, ctx: Ctx) -> Result<(), String> {
    let server = Server::http(addr).map_err(|e| e.to_string())?;
    for request in server.incoming_requests() {
        let url = request.url().to_string();
        let (path, query) = match url.split_once('?') {
            Some((p, q)) => (p.to_string(), parse_query(q)),
            None => (url.clone(), HashMap::new()),
        };
        match request.method() {
            Method::Get => handle_get(request, &ctx, &path, &query),
            Method::Post => handle_post(request, &ctx, &path),
            _ => {
                let _ = request.respond(Response::empty(405));
            }
        }
    }
    Ok(())
}

/// Initial indexing at startup, in the background.
pub fn start_initial_index(ctx: &Ctx) {
    index::start(&ctx.index, ctx.home.clone(), ctx.index_db(), ctx.index_config());
}
