//! Claude and Cursor configuration files: view and, when safe, edit.
//! Equivalente a config_files.py.
//!
//! - The API only accepts identifiers from the CATALOG, never paths.
//! - Editable = candidate AND no credentials in the current content.
//! - Read-only: shown masked (values of keys such as token or secret appear as «••••»).
//! - On save: valid JSON, no new credentials, a previous copy and an atomic write.

use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::{Map, Value};

use crate::fsutil::human_size;
use crate::fsutil::mtime_secs;

pub const MASK: &str = "••••";

const SECRET_KEY: &str =
    r"(?i)token|secret|passw|pwd|api[_-]?key|apikey|authorization|auth|cookie|credential|private[_-]?key|access[_-]?key";
const SECRET_VALUE: &str =
    r"sk-[A-Za-z0-9_-]{16,}|ghp_[A-Za-z0-9]{20,}|xox[abp]-[A-Za-z0-9-]{10,}|AKIA[0-9A-Z]{16}|-----BEGIN [A-Z ]*PRIVATE KEY-----";
const SECRET_TEXT_LINE: &str = r"(?im)^(\s*[\w.-]*(?:token|secret|password|api[_-]?key)[\w.-]*\s*[:=]\s*)(\S.*)$";

pub struct Entry {
    pub id: &'static str,
    pub agent: &'static str,
    pub format: &'static str,
    pub editable: bool,
    pub label: &'static str,
    pub path: &'static str,
}

pub const CATALOG: &[Entry] = &[
    Entry { id: "claude-settings", agent: "claude", format: "json", editable: true, label: "Claude Code settings", path: ".claude/settings.json" },
    Entry { id: "claude-memory", agent: "claude", format: "text", editable: true, label: "Global memory (CLAUDE.md)", path: ".claude/CLAUDE.md" },
    Entry { id: "claude-state", agent: "claude", format: "json", editable: false, label: "Project registry and MCP servers", path: ".claude.json" },
    Entry { id: "claude-desktop", agent: "claude", format: "json", editable: false, label: "Claude Desktop configuration", path: "Library/Application Support/Claude/claude_desktop_config.json" },
    Entry { id: "cursor-hooks", agent: "cursor", format: "json", editable: true, label: "Cursor hooks", path: ".cursor/hooks.json" },
    Entry { id: "cursor-argv", agent: "cursor", format: "json", editable: true, label: "Cursor startup flags", path: ".cursor/argv.json" },
    Entry { id: "cursor-cli", agent: "cursor", format: "json", editable: false, label: "CLI configuration (includes the session)", path: ".cursor/cli-config.json" },
    Entry { id: "cursor-mcp", agent: "cursor", format: "json", editable: false, label: "Cursor MCP servers", path: ".cursor/mcp.json" },
];

fn lookup(config_id: &str) -> Result<&'static Entry, String> {
    CATALOG
        .iter()
        .find(|e| e.id == config_id)
        .ok_or_else(|| format!("Unknown configuration: {config_id}"))
}

fn file_of(home: &Path, entry: &Entry) -> PathBuf {
    home.join(entry.path)
}

fn has_value(value: &Value) -> bool {
    match value {
        Value::String(s) => !s.trim().is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        Value::Bool(b) => *b,
        Value::Null => false,
        Value::Number(_) => true,
    }
}

fn key_is_secret(key: &str) -> bool {
    Regex::new(SECRET_KEY).expect("valid regex").is_match(key)
}

/// Paths (a.b.c) of the values that look like credentials.
pub fn json_secret_paths(node: &Value, prefix: &str, out: &mut Vec<String>) {
    match node {
        Value::Object(map) => {
            for (key, value) in map {
                let path = if prefix.is_empty() { key.clone() } else { format!("{prefix}.{key}") };
                if key_is_secret(key) && has_value(value) {
                    out.push(path);
                } else {
                    json_secret_paths(value, &path, out);
                }
            }
        }
        Value::Array(items) => {
            for (i, value) in items.iter().enumerate() {
                json_secret_paths(value, &format!("{prefix}[{i}]"), out);
            }
        }
        Value::String(s) => {
            if Regex::new(SECRET_VALUE).expect("valid regex").is_match(s) {
                out.push(prefix.to_string());
            }
        }
        _ => {}
    }
}

fn mask_json(node: &Value, key: Option<&str>) -> Value {
    match node {
        Value::Object(map) => Value::Object(map.iter().map(|(k, v)| (k.clone(), mask_json(v, Some(k)))).collect::<Map<_, _>>()),
        Value::Array(items) => Value::Array(items.iter().map(|v| mask_json(v, None)).collect()),
        Value::String(s) => {
            if key.is_some_and(key_is_secret) && has_value(node) {
                Value::String(MASK.into())
            } else {
                Value::String(mask_secret_values(s))
            }
        }
        other => {
            if key.is_some_and(key_is_secret) && has_value(other) {
                Value::String(MASK.into())
            } else {
                other.clone()
            }
        }
    }
}

/// Replaces values shaped like credentials with the mask.
pub fn mask_secret_values(text: &str) -> String {
    Regex::new(SECRET_VALUE).expect("valid regex").replace_all(text, MASK).into_owned()
}

fn mask_text(raw: &str) -> String {
    let lines = Regex::new(SECRET_TEXT_LINE).expect("valid regex").replace_all(raw, |caps: &regex::Captures| {
        format!("{}{}", &caps[1], MASK)
    });
    mask_secret_values(&lines)
}

fn parse(raw: &str, format: &str) -> Result<Option<Value>, String> {
    if format != "json" {
        return Ok(None);
    }
    serde_json::from_str::<Value>(raw)
        .map(Some)
        .map_err(|e| format!("Invalid JSON (line {}, column {})", e.line(), e.column()))
}

fn secret_reasons(raw: &str, format: &str) -> Vec<String> {
    match parse(raw, format) {
        Err(error) => vec![error],
        Ok(Some(data)) => {
            let mut paths = Vec::new();
            json_secret_paths(&data, "", &mut paths);
            if paths.is_empty() {
                Vec::new()
            } else {
                let shown: Vec<String> = paths.iter().take(4).cloned().collect();
                let more = if paths.len() > 4 { " …" } else { "" };
                vec![format!("Contains credentials in: {}{more}", shown.join(", "))]
            }
        }
        Ok(None) => {
            let plain = Regex::new(SECRET_VALUE).expect("valid regex").is_match(raw)
                || Regex::new(SECRET_TEXT_LINE).expect("valid regex").is_match(raw);
            if plain {
                vec!["Contains credentials in plain text".into()]
            } else {
                Vec::new()
            }
        }
    }
}

fn view(raw: &str, format: &str, editable: bool) -> String {
    if editable {
        return raw.to_string();
    }
    match parse(raw, format) {
        Ok(Some(data)) => serde_json::to_string_pretty(&mask_json(&data, None)).unwrap_or_default(),
        _ => mask_text(raw),
    }
}

/// (editable, reasons, current content) of an existing file.
fn status(home: &Path, entry: &Entry) -> Result<(bool, Vec<String>, String), String> {
    let raw = String::from_utf8_lossy(&fs::read(file_of(home, entry)).map_err(|e| e.to_string())?).into_owned();
    let mut reasons = secret_reasons(&raw, entry.format);
    if !entry.editable {
        reasons.insert(0, "Read-only by design: contains the session or server credentials".into());
    }
    Ok((reasons.is_empty(), reasons, raw))
}

pub fn list(home: &Path) -> Vec<Value> {
    let mut items = Vec::new();
    for entry in CATALOG {
        let path = file_of(home, entry);
        if !path.is_file() {
            continue;
        }
        let Ok((editable, reasons, _)) = status(home, entry) else {
            continue;
        };
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        items.push(serde_json::json!({
            "id": entry.id, "agent": entry.agent, "label": entry.label,
            "path": path.to_string_lossy(), "format": entry.format,
            "editable": editable, "reasons": reasons,
            "size_human": human_size(meta.len()), "last_modified": mtime_secs(&meta),
        }));
    }
    items
}

pub fn read(home: &Path, config_id: &str) -> Result<Value, String> {
    let entry = lookup(config_id)?;
    let path = file_of(home, entry);
    if !path.is_file() {
        return Err(format!("{} does not exist on this machine", entry.path));
    }
    let (editable, reasons, raw) = status(home, entry)?;
    let mtime_ns = fs::metadata(&path)
        .map_err(|e| e.to_string())?
        .modified()
        .map_err(|e| e.to_string())?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos() as u64;
    Ok(serde_json::json!({
        "id": config_id, "label": entry.label, "path": path.to_string_lossy(),
        "format": entry.format, "editable": editable, "reasons": reasons,
        "content": view(&raw, entry.format, editable), "mtime_ns": mtime_ns,
    }))
}

pub fn write(home: &Path, config_id: &str, content: &str, expected_mtime_ns: u64, backup_dir: &Path) -> Result<Value, String> {
    let entry = lookup(config_id)?;
    let path = file_of(home, entry);
    if !path.is_file() {
        return Err(format!("{} does not exist on this machine", entry.path));
    }
    let current = read(home, config_id)?;
    if current["mtime_ns"].as_u64() != Some(expected_mtime_ns) {
        return Err("The file changed since you opened it. Reload before saving.".into());
    }
    let (editable, _, _) = status(home, entry)?;
    if !editable {
        return Err("This file cannot be edited from the dashboard".into());
    }
    parse(content, entry.format)?;
    if !secret_reasons(content, entry.format).is_empty() {
        return Err("The new content includes credentials: it is not saved from here".into());
    }

    let backup = crate::backup::backup_file(&path, backup_dir, config_id)?;
    let tmp = path.with_file_name(format!(
        "{}.dashboard-tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    fs::write(&tmp, content).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    let after = read(home, config_id)?;
    Ok(serde_json::json!({"ok": true, "backup": backup.to_string_lossy(), "mtime_ns": after["mtime_ns"]}))
}
