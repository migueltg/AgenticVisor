//! Adapters: each tool knows where it keeps its data. Discovery
//! generic covers any other AI-related folder.

pub mod claude_code;
pub mod claude_desktop;
pub mod cursor_cli;
pub mod cursor_desktop;
pub mod generic;

pub mod codex;
pub mod hermes;

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::entry::Entry;
use crate::fsutil::{dir_stats, mtime_secs};

/// Direct children of `dir`, sorted; empty if the folder cannot be read.
pub fn sorted_children(dir: &Path) -> Vec<PathBuf> {
    let mut items: Vec<PathBuf> = fs::read_dir(dir).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    items.sort();
    items
}

/// Entry for one top-level item of a tool folder: a file or a whole folder.
/// Symlinks are skipped: they point at data that belongs to something else.
pub fn child_entry(tool: &str, item: &Path, category: &str, sensitive: bool, note: &str) -> Option<Entry> {
    let meta = fs::symlink_metadata(item).ok()?;
    if meta.file_type().is_symlink() {
        return None;
    }
    let (size, count, mtime) = if meta.is_dir() {
        dir_stats(item, &[])
    } else {
        (meta.len(), 1, mtime_secs(&meta))
    };
    let id = item.to_string_lossy().into_owned();
    Some(
        Entry::new(&id, tool, category, &id)
            .with_stats(size, count, mtime)
            .sensitive(sensitive)
            .note(note),
    )
}

/// Messages the agent injects itself. They are not what the user typed, so they
/// are skipped when looking for a conversation title.
const INJECTED_PREFIXES: &[&str] = &["<environment_context>", "<user_instructions>"];

/// First message written by the user in a JSON Lines conversation, read from the
/// first `max_lines` lines. Accepts a flat record ({"role", "content"}) and a
/// wrapped one ({"payload": {"role", "content"}}).
pub fn first_user_text(path: &Path, max_lines: usize) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    for line in BufReader::new(file).lines().take(max_lines).map_while(Result::ok) {
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        for node in [Some(&record), record.get("payload")].into_iter().flatten() {
            if node.get("role").and_then(Value::as_str) != Some("user") {
                continue;
            }
            if let Some(text) = message_text(node.get("content")) {
                if !INJECTED_PREFIXES.iter().any(|p| text.trim_start().starts_with(p)) {
                    return Some(text);
                }
            }
        }
    }
    None
}

/// Text of a message: a plain string, or the text parts of a list of blocks.
fn message_text(content: Option<&Value>) -> Option<String> {
    match content? {
        Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
        Value::Array(parts) => {
            let text: Vec<&str> = parts.iter().filter_map(|p| p.get("text").and_then(Value::as_str)).collect();
            let joined = text.join(" ");
            (!joined.trim().is_empty()).then_some(joined)
        }
        _ => None,
    }
}

/// Working directory recorded in the first `max_lines` lines of a conversation.
pub fn session_cwd(path: &Path, max_lines: usize) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    for line in BufReader::new(file).lines().take(max_lines).map_while(Result::ok) {
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        for node in [Some(&record), record.get("payload")].into_iter().flatten() {
            if let Some(cwd) = node.get("cwd").and_then(Value::as_str).filter(|c| !c.is_empty()) {
                return Some(cwd.to_string());
            }
        }
    }
    None
}
