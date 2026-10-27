//! Adapter for the Codex CLI (~/.codex).
//!
//! Conversations are JSON Lines "rollout" files under sessions/. The reader is
//! tolerant: it takes the title and the working directory from the records when
//! they are there, and falls back to the file size and date otherwise. No rollout
//! from a real install was available when this was written, so the record layout
//! is not verified against real data yet.

use std::fs;
use std::path::{Path, PathBuf};

use crate::adapters::{child_entry, first_user_text, session_cwd, sorted_children};
use crate::entry::Entry;
use crate::fsutil::{classify_by_name, mtime_secs};
use crate::titles::clean_title;

const TOOL: &str = "codex";

/// Known top-level items of ~/.codex: (name, category, sensitive, note).
const KNOWN_ITEMS: &[(&str, &str, bool, &str)] = &[
    ("memories", "other", false, "Memories the agent keeps across sessions"),
    ("skills", "skills", false, "Installed skills"),
    ("tmp", "cache", false, "Temporary files"),
    ("auth.json", "config", true, "Authentication credentials"),
    ("config.toml", "config", true, "Configuration (may contain tokens)"),
    ("AGENTS.md", "config", false, "Global instructions for the agent"),
];

pub fn scan(home: &Path) -> Vec<Entry> {
    let root = home.join(".codex");
    if !root.is_dir() {
        return Vec::new();
    }
    let mut out = Vec::new();

    let mut rollouts = Vec::new();
    collect_rollouts(&root.join("sessions"), &mut rollouts);
    out.extend(rollouts.iter().filter_map(|file| session_entry(file)));

    for item in sorted_children(&root) {
        if item.file_name().is_some_and(|n| n == "sessions") {
            continue;
        }
        let name = item.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let (category, sensitive, note) = match KNOWN_ITEMS.iter().find(|(n, ..)| *n == name) {
            Some((_, category, sensitive, note)) => (category.to_string(), *sensitive, note.to_string()),
            None => {
                let (category, sensitive) = classify_by_name(&name);
                (category.to_string(), sensitive, String::new())
            }
        };
        out.extend(child_entry(TOOL, &item, &category, sensitive, &note));
    }
    out
}

/// Finds the rollout files, which may sit in year/month/day folders.
fn collect_rollouts(dir: &Path, out: &mut Vec<PathBuf>) {
    for item in sorted_children(dir) {
        let Ok(meta) = fs::symlink_metadata(&item) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            collect_rollouts(&item, out);
            continue;
        }
        let is_rollout = item.file_name().is_some_and(|n| n.to_string_lossy().starts_with("rollout-"));
        if is_rollout && item.extension().is_some_and(|e| e == "jsonl") {
            out.push(item);
        }
    }
}

fn session_entry(file: &Path) -> Option<Entry> {
    let meta = fs::metadata(file).ok()?;
    let id = file.to_string_lossy().into_owned();
    let title = first_user_text(file, 400)
        .map(|text| clean_title(&text))
        .unwrap_or_else(|| "(no user messages)".to_string());
    let mut entry = Entry::new(&id, TOOL, "sessions", &id)
        .with_stats(meta.len(), 1, mtime_secs(&meta))
        .title(title)
        .note("Codex conversation");
    if let Some(cwd) = session_cwd(file, 400) {
        entry = entry.note(format!("Project: {cwd}")).project(cwd);
    }
    Some(entry)
}
