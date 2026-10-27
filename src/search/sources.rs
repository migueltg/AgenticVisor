//! Text readers for the search and the preview.
//!
//! Each session is described by a `SessionSource`: title, version (changes when
//! the content changes, so only modified items are re-indexed) and a function that
//! lazily produces the messages (kind, text).

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use crate::fsutil::{dir_stats, sql_bytes};
use crate::titles::{
    claude_code_ai_title, claude_code_fallback_title, claude_code_history_index, claude_transcript_title,
    clean_title, cursor_chat_title, cursor_transcript_title,
};

pub type Message = (String, String);

/// Bump this number when what is extracted from any source changes.
pub const EXTRACTOR_VERSION: &str = "2";

const SKIP_KEYS: &[&str] = &[
    "type", "id", "role", "signature", "providerOptions", "toolUseId", "tool_use_id", "toolCallId",
    "requestId", "timestamp", "uuid", "parentUuid", "sessionId", "cacheKey", "data",
];

const CURSOR_BUBBLE_EXTRA_SKIP: &[&str] = &[
    "richText", "codeBlocks", "suggestedCodeBlocks", "attachedCodeChunks", "codebaseContextChunks",
    "attachedFileCodeChunksMetadataOnly", "relevantFiles", "recentlyViewedFiles", "diffsForCompressingFiles",
    "gitDiffs", "fileDiffTrajectories", "diffHistories", "humanChanges", "attachedHumanChanges",
    "editTrailContexts", "context", "contextPieces", "projectLayouts", "workspaceUris", "capabilityContexts",
    "lints", "approximateLintErrors", "multiFileLinterErrors", "webReferences", "docsReferences",
    "aiWebSearchResults", "modelInfo", "mcpDescriptors", "supportedTools", "capabilities", "allThinkingBlocks",
];

const TEXT_SUFFIXES: &[&str] = &[
    "", "txt", "md", "json", "log", "csv", "xml", "html", "yaml", "yml", "sql", "sh", "py", "ts", "tsx", "js",
    "jsx", "css", "toml", "ini",
];

pub struct SessionSource {
    pub key: String,
    pub tool: String,
    pub path: String,
    pub title: String,
    pub version: String,
    pub read: Box<dyn Fn() -> Vec<Message> + Send>,
}

/// Collects the strings of a nested JSON value, without the keys in `skip`.
pub fn collect_text(node: &Value, skip: &HashSet<&str>) -> String {
    let mut parts = Vec::new();
    collect_into(node, skip, &mut parts);
    parts.join("\n")
}

fn is_blob(text: &str) -> bool {
    text.chars().count() > 500 && !text.chars().take(500).any(char::is_whitespace)
}

fn collect_into(node: &Value, skip: &HashSet<&str>, parts: &mut Vec<String>) {
    match node {
        Value::String(s) => {
            if !s.trim().is_empty() && !is_blob(s) {
                parts.push(s.clone());
            }
        }
        Value::Array(items) => items.iter().for_each(|i| collect_into(i, skip, parts)),
        Value::Object(map) => {
            for (key, value) in map {
                if !skip.contains(key.as_str()) {
                    collect_into(value, skip, parts);
                }
            }
        }
        _ => {}
    }
}

fn default_skip() -> HashSet<&'static str> {
    SKIP_KEYS.iter().copied().collect()
}

fn bubble_skip() -> HashSet<&'static str> {
    SKIP_KEYS.iter().chain(CURSOR_BUBBLE_EXTRA_SKIP.iter()).copied().collect()
}

fn json_lines(path: &Path) -> Vec<Value> {
    let Ok(file) = File::open(path) else {
        return Vec::new();
    };
    BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        .filter(Value::is_object)
        .collect()
}

/// Claude Code format (projects/, transcripts/ and Claude Desktop). Covers the
/// schema with "message" and the flat transcripts/ layout with "content" at the root.
pub fn iter_claude_messages(path: &Path) -> Vec<Message> {
    let skip = default_skip();
    let mut out = Vec::new();
    for d in json_lines(path) {
        let role = d.get("type").and_then(Value::as_str).unwrap_or("");
        if role != "user" && role != "assistant" {
            continue;
        }
        let message = match d.get("message") {
            Some(m) if m.is_object() => m,
            _ => &d,
        };
        let content = message.get("content");
        if let Some(text) = content.and_then(Value::as_str) {
            out.push((role.to_string(), text.to_string()));
            continue;
        }
        for block in content.and_then(Value::as_array).into_iter().flatten() {
            let Some(block_type) = block.get("type").and_then(Value::as_str) else {
                continue;
            };
            match block_type {
                "text" => out.push((role.to_string(), block.get("text").and_then(Value::as_str).unwrap_or("").to_string())),
                "thinking" => out.push((
                    "thinking".to_string(),
                    block.get("thinking").and_then(Value::as_str).unwrap_or("").to_string(),
                )),
                "tool_use" => out.push(("tool".to_string(), collect_text(block.get("input").unwrap_or(&Value::Null), &skip))),
                "tool_result" => out.push(("tool".to_string(), collect_text(block.get("content").unwrap_or(&Value::Null), &skip))),
                _ => {}
            }
        }
    }
    out
}

/// Cursor agent transcripts (agent-transcripts/).
pub fn iter_cursor_transcript(path: &Path) -> Vec<Message> {
    let skip = default_skip();
    let mut out = Vec::new();
    for d in json_lines(path) {
        let role = d.get("role").and_then(Value::as_str).unwrap_or("");
        if role != "user" && role != "assistant" {
            continue;
        }
        for block in d.pointer("/message/content").and_then(Value::as_array).into_iter().flatten() {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => out.push((role.to_string(), block.get("text").and_then(Value::as_str).unwrap_or("").to_string())),
                Some("tool_use") => out.push(("tool".to_string(), collect_text(block.get("input").unwrap_or(&Value::Null), &skip))),
                _ => {}
            }
        }
    }
    out
}

fn read_only(path: &Path) -> Option<Connection> {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX).ok()
}

fn blob_json(raw: &[u8]) -> Option<Value> {
    serde_json::from_slice::<Value>(raw).ok()
}

/// Cursor CLI store.db: one row per message. The system prompt is skipped,
/// as is the injected <user_info>, which the user did not write.
pub fn iter_cursor_chat(store_db: &Path) -> Vec<Message> {
    let skip = default_skip();
    let Some(con) = read_only(store_db) else {
        return Vec::new();
    };
    let Ok(mut stmt) = con.prepare("SELECT data FROM blobs ORDER BY id") else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map([], |row| row.get::<_, rusqlite::types::Value>(0).map(|v| sql_bytes(v).unwrap_or_default())) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for raw in rows.flatten() {
        let Some(d) = blob_json(&raw) else {
            continue;
        };
        if !d.is_object() {
            continue;
        }
        let role = d.get("role").and_then(Value::as_str).unwrap_or("");
        if role == "system" {
            continue;
        }
        let kind = match role {
            "user" => "user",
            "assistant" => "assistant",
            _ => "tool",
        };
        match d.get("content") {
            Some(Value::String(text)) => {
                if !text.starts_with("<user_info>") {
                    out.push((kind.to_string(), text.clone()));
                }
            }
            Some(Value::Array(blocks)) => {
                for block in blocks {
                    if block.get("type").and_then(Value::as_str) == Some("text") {
                        out.push((kind.to_string(), block.get("text").and_then(Value::as_str).unwrap_or("").to_string()));
                    } else {
                        out.push(("tool".to_string(), collect_text(block, &skip)));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// A Cursor Desktop chat: bubbleId:<chat>:<message> rows of state.vscdb.
/// type 1 = user, 2 = assistant.
pub fn iter_cursor_bubbles(state_db: &Path, composer_id: &str) -> Vec<Message> {
    let skip = bubble_skip();
    let Some(con) = read_only(state_db) else {
        return Vec::new();
    };
    let Ok(mut stmt) = con.prepare("SELECT value FROM cursorDiskKV WHERE key >= ?1 AND key < ?2") else {
        return Vec::new();
    };
    let lower = format!("bubbleId:{composer_id}:");
    let upper = format!("bubbleId:{composer_id};");
    let Ok(rows) = stmt.query_map([lower, upper], |row| row.get::<_, rusqlite::types::Value>(0).map(|v| sql_bytes(v).unwrap_or_default())) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for raw in rows.flatten() {
        let Some(bubble) = blob_json(&raw) else {
            continue;
        };
        if !bubble.is_object() {
            continue;
        }
        let role = if bubble.get("type").and_then(Value::as_u64) == Some(1) { "user" } else { "assistant" };
        if let Some(text) = bubble.get("text").and_then(Value::as_str) {
            if !text.is_empty() {
                out.push((role.to_string(), text.to_string()));
            }
        }
        if let Some(text) = bubble.pointer("/thinking/text").and_then(Value::as_str) {
            if !text.is_empty() {
                out.push(("thinking".to_string(), text.to_string()));
            }
        }
        let tool_text = collect_text(
            &Value::Array(vec![
                bubble.get("toolFormerData").cloned().unwrap_or(Value::Null),
                bubble.get("toolResults").cloned().unwrap_or(Value::Null),
            ]),
            &skip,
        );
        if !tool_text.is_empty() {
            out.push(("tool".to_string(), tool_text));
        }
    }
    out
}

/// Tool outputs saved next to a transcript (tool-results/).
fn text_files(folder: &Path) -> Vec<Message> {
    let mut files = Vec::new();
    collect_files(folder, &mut files);
    files.sort();
    let mut out = Vec::new();
    for file in files {
        let suffix = file.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        if TEXT_SUFFIXES.contains(&suffix.as_str()) {
            if let Ok(bytes) = fs::read(&file) {
                out.push(("tool".to_string(), String::from_utf8_lossy(&bytes).into_owned()));
            }
        }
    }
    out
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(read) = fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(t) if t.is_dir() => collect_files(&path, out),
            Ok(t) if t.is_file() => out.push(path),
            _ => {}
        }
    }
}

/// Signature of a list of files: changes if any of them changes.
pub fn files_version(files: &[PathBuf]) -> String {
    let mut count = 0u64;
    let mut total = 0u64;
    let mut latest = 0i128;
    for f in files {
        if let Ok(meta) = fs::metadata(f) {
            count += 1;
            total += meta.len();
            if let Ok(modified) = meta.modified() {
                if let Ok(d) = modified.duration_since(std::time::UNIX_EPOCH) {
                    latest = latest.max(d.as_nanos() as i128);
                }
            }
        }
    }
    format!("{count}:{total}:{latest}")
}

fn sorted_children(dir: &Path) -> Vec<PathBuf> {
    let mut items: Vec<PathBuf> = fs::read_dir(dir).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    items.sort();
    items
}

fn glob_jsonl(dir: &Path) -> Vec<PathBuf> {
    sorted_children(dir)
        .into_iter()
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "jsonl"))
        .collect()
}

fn rglob_jsonl(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_jsonl(dir, &mut out);
    out.sort();
    out
}

fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>) {
    for item in sorted_children(dir) {
        let Ok(t) = fs::symlink_metadata(&item).map(|m| m.file_type()) else {
            continue;
        };
        if t.is_dir() {
            collect_jsonl(&item, out);
        } else if t.is_file() && item.extension().is_some_and(|e| e == "jsonl") {
            out.push(item);
        }
    }
}

fn read_claude_session(files: Vec<PathBuf>, sidecar: Option<PathBuf>) -> Vec<Message> {
    let mut out = Vec::new();
    for f in &files {
        out.extend(iter_claude_messages(f));
    }
    if let Some(sidecar) = sidecar {
        if sidecar.is_dir() {
            out.extend(text_files(&sidecar));
        }
    }
    out
}

fn claude_code_sessions(home: &Path, out: &mut Vec<SessionSource>) {
    let history = claude_code_history_index(home);
    let projects = home.join(".claude").join("projects");
    for project in sorted_children(&projects).into_iter().filter(|p| p.is_dir()) {
        for jsonl in glob_jsonl(&project) {
            let stem = jsonl.file_stem().unwrap_or_default().to_string_lossy().into_owned();
            let sidecar = project.join(&stem);
            let (mut files, sidecar_opt, sidecar_size, sidecar_mtime) = if sidecar.is_dir() {
                let mut files = vec![jsonl.clone()];
                files.extend(rglob_jsonl(&sidecar));
                let (size, _count, mtime) = dir_stats(&sidecar, &[]);
                (files, Some(sidecar.clone()), size, mtime)
            } else {
                (vec![jsonl.clone()], None, 0, 0.0)
            };
            files.sort_by(|a, b| (a != &jsonl).cmp(&(b != &jsonl)).then(a.cmp(b)));
            let title = claude_code_ai_title(&jsonl)
                .or_else(|| history.get(&stem).cloned())
                .or_else(|| claude_code_fallback_title(&jsonl))
                .unwrap_or_else(|| "(untitled)".to_string());
            let version = format!("{}:{}:{}", files_version(&files), sidecar_size, sidecar_mtime);
            let read_files = files.clone();
            out.push(SessionSource {
                key: jsonl.to_string_lossy().into_owned(),
                tool: "claude-code".into(),
                path: jsonl.to_string_lossy().into_owned(),
                title,
                version,
                read: Box::new(move || read_claude_session(read_files.clone(), sidecar_opt.clone())),
            });
        }
    }

    let transcripts = home.join(".claude").join("transcripts");
    for jsonl in glob_jsonl(&transcripts) {
        let title = claude_transcript_title(&jsonl).unwrap_or_else(|| "(untitled)".to_string());
        let version = files_version(std::slice::from_ref(&jsonl));
        let file = jsonl.clone();
        out.push(SessionSource {
            key: jsonl.to_string_lossy().into_owned(),
            tool: "claude-code".into(),
            path: jsonl.to_string_lossy().into_owned(),
            title,
            version,
            read: Box::new(move || iter_claude_messages(&file)),
        });
    }
}

/// Claude Desktop titles: local_<id>.json carries the title and the Claude Code session id.
fn desktop_titles(root: &Path) -> (std::collections::HashMap<String, String>, std::collections::HashMap<String, String>) {
    let mut by_cli = std::collections::HashMap::new();
    let mut by_local = std::collections::HashMap::new();
    for account in sorted_children(root).into_iter().filter(|p| p.is_dir()) {
        for space in sorted_children(&account).into_iter().filter(|p| p.is_dir()) {
            for meta in sorted_children(&space) {
                let name = meta.file_name().unwrap_or_default().to_string_lossy().into_owned();
                if !(name.starts_with("local_") && name.ends_with(".json")) {
                    continue;
                }
                let Some(data) = fs::read_to_string(&meta).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) else {
                    continue;
                };
                let Some(title) = data.get("title").and_then(Value::as_str).filter(|t| !t.trim().is_empty()) else {
                    continue;
                };
                let title = clean_title(title);
                let stem = name.trim_end_matches(".json").to_string();
                by_local.insert(stem, title.clone());
                if let Some(cli) = data.get("cliSessionId").and_then(Value::as_str) {
                    by_cli.insert(cli.to_string(), title);
                }
            }
        }
    }
    (by_cli, by_local)
}

fn claude_desktop_sessions(home: &Path, out: &mut Vec<SessionSource>) {
    let root = home.join("Library").join("Application Support").join("Claude").join("local-agent-mode-sessions");
    if !root.is_dir() {
        return;
    }
    let (by_cli, by_local) = desktop_titles(&root);

    // Loose transcripts in the account/workspace: Claude Code sessions started from the app.
    let mut top_level = Vec::new();
    for account in sorted_children(&root).into_iter().filter(|p| p.is_dir()) {
        for space in sorted_children(&account).into_iter().filter(|p| p.is_dir()) {
            top_level.extend(glob_jsonl(&space));
        }
    }
    top_level.sort();
    for jsonl in top_level {
        let stem = jsonl.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        let title = by_cli
            .get(&stem)
            .cloned()
            .or_else(|| claude_code_ai_title(&jsonl))
            .or_else(|| claude_code_fallback_title(&jsonl))
            .unwrap_or_else(|| "(untitled)".to_string());
        let version = files_version(std::slice::from_ref(&jsonl));
        let file = jsonl.clone();
        out.push(SessionSource {
            key: jsonl.to_string_lossy().into_owned(),
            tool: "claude-desktop".into(),
            path: jsonl.to_string_lossy().into_owned(),
            title,
            version,
            read: Box::new(move || iter_claude_messages(&file)),
        });
    }

    // Each local_<id>/ is a session with its audit.jsonl and its subagents.
    let mut locals = Vec::new();
    for account in sorted_children(&root).into_iter().filter(|p| p.is_dir()) {
        for space in sorted_children(&account).into_iter().filter(|p| p.is_dir()) {
            for local in sorted_children(&space) {
                let name = local.file_name().unwrap_or_default().to_string_lossy().into_owned();
                if local.is_dir() && name.starts_with("local_") {
                    locals.push((local, name));
                }
            }
        }
    }
    for (local, name) in locals {
        let files = rglob_jsonl(&local);
        if files.is_empty() {
            continue;
        }
        let main = files
            .iter()
            .find(|f| f.file_name().is_some_and(|n| n == "audit.jsonl"))
            .cloned()
            .unwrap_or_else(|| files[0].clone());
        let title = by_local
            .get(&name)
            .cloned()
            .or_else(|| claude_code_ai_title(&main))
            .or_else(|| claude_code_fallback_title(&main))
            .unwrap_or_else(|| "(untitled)".to_string());
        let version = files_version(&files);
        let read_files = files.clone();
        out.push(SessionSource {
            key: local.to_string_lossy().into_owned(),
            tool: "claude-desktop".into(),
            path: local.to_string_lossy().into_owned(),
            title,
            version,
            read: Box::new(move || read_claude_session(read_files.clone(), None)),
        });
    }
}

fn cursor_cli_sessions(home: &Path, out: &mut Vec<SessionSource>) {
    let projects = home.join(".cursor").join("projects");
    for project in sorted_children(&projects).into_iter().filter(|p| p.is_dir()) {
        for conv in sorted_children(&project.join("agent-transcripts")).into_iter().filter(|p| p.is_dir()) {
            let jsonl = conv.join(format!("{}.jsonl", conv.file_name().unwrap_or_default().to_string_lossy()));
            if !jsonl.is_file() {
                continue;
            }
            let title = cursor_transcript_title(&jsonl).unwrap_or_else(|| "(no user messages)".to_string());
            let version = files_version(std::slice::from_ref(&jsonl));
            let file = jsonl.clone();
            out.push(SessionSource {
                key: jsonl.to_string_lossy().into_owned(),
                tool: "cursor-cli".into(),
                path: conv.to_string_lossy().into_owned(),
                title,
                version,
                read: Box::new(move || iter_cursor_transcript(&file)),
            });
        }
    }

    let chats = home.join(".cursor").join("chats");
    for hash in sorted_children(&chats).into_iter().filter(|p| p.is_dir()) {
        for chat in sorted_children(&hash).into_iter().filter(|p| p.is_dir()) {
            let db = chat.join("store.db");
            if !db.is_file() {
                continue;
            }
            let title = cursor_chat_title(&db).unwrap_or_else(|| "(untitled)".to_string());
            let version = files_version(std::slice::from_ref(&db));
            let file = db.clone();
            out.push(SessionSource {
                key: db.to_string_lossy().into_owned(),
                tool: "cursor-cli".into(),
                path: chat.to_string_lossy().into_owned(),
                title,
                version,
                read: Box::new(move || iter_cursor_chat(&file)),
            });
        }
    }
}

fn cursor_desktop_sessions(home: &Path, out: &mut Vec<SessionSource>) {
    let db = home.join("Library").join("Application Support").join("Cursor").join("User").join("globalStorage").join("state.vscdb");
    if !db.is_file() {
        return;
    }
    let Some(con) = read_only(&db) else {
        return;
    };
    let Ok(mut stmt) = con.prepare("SELECT key, value FROM cursorDiskKV WHERE key >= 'composerData:' AND key < 'composerData;'") else {
        return;
    };
    let Ok(rows) = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, rusqlite::types::Value>(1).map(|v| sql_bytes(v).unwrap_or_default())?))) else {
        return;
    };
    for (key, raw) in rows.flatten() {
        let Some(composer) = blob_json(&raw) else {
            continue;
        };
        if !composer.is_object() {
            continue;
        }
        let headers = composer.get("fullConversationHeadersOnly").and_then(Value::as_array).map(Vec::len).unwrap_or(0);
        let has_map = composer.get("conversationMap").is_some_and(|m| m.as_object().is_some_and(|o| !o.is_empty()));
        if headers == 0 && !has_map {
            continue;
        }
        let Some(cid) = key.split_once(':').map(|(_, c)| c.to_string()) else {
            continue;
        };
        let stamp = composer
            .get("lastUpdatedAt")
            .or_else(|| composer.get("createdAt"))
            .map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()))
            .unwrap_or_default();
        let name = composer
            .get("name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| composer.get("subtitle").and_then(Value::as_str).filter(|s| !s.is_empty()))
            .unwrap_or("(untitled)");
        let db_path = db.clone();
        let chat = cid.clone();
        out.push(SessionSource {
            key: format!("{}#{}", db.to_string_lossy(), cid),
            tool: "cursor-desktop".into(),
            path: db.to_string_lossy().into_owned(),
            title: clean_title(name),
            version: format!("{stamp}:{headers}"),
            read: Box::new(move || iter_cursor_bubbles(&db_path, &chat)),
        });
    }
}

fn plan_sessions(home: &Path, out: &mut Vec<SessionSource>) {
    for (tool, folder) in [
        ("claude-code", home.join(".claude").join("plans")),
        ("cursor-cli", home.join(".cursor").join("plans")),
    ] {
        if !folder.is_dir() {
            continue;
        }
        for md in sorted_children(&folder).into_iter().filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "md")) {
            let text = String::from_utf8_lossy(&fs::read(&md).unwrap_or_default()).into_owned();
            let first = text
                .lines()
                .map(|l| l.trim_matches(|c: char| c == '#' || c == ' ').trim().to_string())
                .find(|l| !l.is_empty());
            let title = match first {
                Some(line) => clean_title(&line),
                None => md.file_stem().unwrap_or_default().to_string_lossy().into_owned(),
            };
            let version = files_version(std::slice::from_ref(&md));
            let file = md.clone();
            out.push(SessionSource {
                key: md.to_string_lossy().into_owned(),
                tool: tool.into(),
                path: md.to_string_lossy().into_owned(),
                title,
                version,
                read: Box::new(move || {
                    vec![("assistant".to_string(), String::from_utf8_lossy(&fs::read(&file).unwrap_or_default()).into_owned())]
                }),
            });
        }
    }
}

/// All the sessions the search must know about.
pub fn all_sources(home: &Path) -> Vec<SessionSource> {
    let mut out = Vec::new();
    claude_code_sessions(home, &mut out);
    claude_desktop_sessions(home, &mut out);
    cursor_cli_sessions(home, &mut out);
    cursor_desktop_sessions(home, &mut out);
    plan_sessions(home, &mut out);
    out
}
