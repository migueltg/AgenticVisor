//! Adapter for the Hermes agent (~/.hermes).
//!
//! Conversations are JSON Lines files in sessions/, one record per message with
//! "role", "content" and "timestamp" (checked against a real install). Hermes does
//! not record a working directory, so its sessions have no project.

use std::path::Path;

use crate::adapters::{child_entry, first_user_text, sorted_children};
use crate::entry::Entry;
use crate::fsutil::{classify_by_name, mtime_secs};
use crate::titles::clean_title;

const TOOL: &str = "hermes";

/// Known top-level items of ~/.hermes: (name, category, sensitive, note).
const KNOWN_ITEMS: &[(&str, &str, bool, &str)] = &[
    ("skills", "skills", false, "Installed skills"),
    ("cache", "cache", false, "Cache"),
    ("audio_cache", "cache", false, "Cached audio"),
    ("bootstrap-cache", "cache", false, "Bootstrap cache"),
    ("backups", "other", false, "Backups of the configuration"),
    (".curator_backups", "other", false, "Curator backups"),
    (".hermes_history", "sessions", false, "Command history"),
    ("auth.json", "config", true, "Authentication credentials"),
    ("hermes-agent", "other", false, "Hermes install: code and virtualenv"),
];

pub fn scan(home: &Path) -> Vec<Entry> {
    let root = home.join(".hermes");
    if !root.is_dir() {
        return Vec::new();
    }
    let mut out = Vec::new();

    for item in sorted_children(&root.join("sessions")) {
        if let Some(entry) = session_entry(&item) {
            out.push(entry);
        }
    }

    for item in sorted_children(&root) {
        if item.file_name().is_some_and(|n| n == "sessions") {
            continue;
        }
        let name = item.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let (category, sensitive, note) = match KNOWN_ITEMS.iter().find(|(n, ..)| *n == name) {
            Some((_, category, sensitive, note)) => (category.to_string(), *sensitive, note.to_string()),
            // Configuration files, including the ".bak" copies, and the environment file with keys.
            None if name.starts_with("config.yaml") || name.starts_with(".env") => {
                ("config".to_string(), true, "May contain API keys".to_string())
            }
            None => {
                let (category, sensitive) = classify_by_name(&name);
                (category.to_string(), sensitive, String::new())
            }
        };
        out.extend(child_entry(TOOL, &item, &category, sensitive, &note));
    }
    out
}

fn session_entry(file: &Path) -> Option<Entry> {
    let meta = std::fs::symlink_metadata(file).ok()?;
    if !meta.is_file() {
        return None;
    }
    let name = file.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let id = file.to_string_lossy().into_owned();
    let mut entry = Entry::new(&id, TOOL, "sessions", &id).with_stats(meta.len(), 1, mtime_secs(&meta));
    if file.extension().is_some_and(|e| e == "jsonl") {
        let title = first_user_text(file, 200)
            .map(|text| clean_title(&text))
            .unwrap_or_else(|| "(no user messages)".to_string());
        entry = entry.title(title).note("Hermes conversation");
    } else if name.starts_with("request_dump_") {
        // Raw request dumps can contain prompts and keys, so they are shown as sensitive.
        entry = entry.sensitive(true).note("Request dump: may contain prompts or keys");
    } else {
        entry = entry.note("Hermes session file");
    }
    Some(entry)
}
