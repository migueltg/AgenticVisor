//! Adapter for the Cursor desktop app (~/Library/Application Support/Cursor).
//! Equivalente a adapters/cursor_desktop.py.
//!
//! state.vscdb is the Cursor Desktop chat database: it is not a cache, deleting it loses
//! the conversations. That is why it is classified as sessions and is informational only.

use std::fs;
use std::path::{Path, PathBuf};

use percent_encoding::percent_decode_str;
use rusqlite::{Connection, OpenFlags};

use crate::entry::Entry;
use crate::fsutil::{classify_by_name, dir_stats, mtime_secs};

const KNOWN_SUBDIRS: &[(&str, &str, bool, &str)] = &[
    ("AgentStores", "sessions", false, "State of the agents in the desktop app"),
    ("Backups", "cache", false, "Automatic backups of edited files"),
    ("CachedConfigurations", "cache", false, ""),
    ("CachedExtensionVSIXs", "cache", false, "Extensiones descargadas, cacheadas"),
    ("CachedProfilesData", "cache", false, ""),
    ("DIPS", "cache", false, "Chromium internal state (tracking prevention)"),
    ("Local State", "config", true, "Includes local encryption keys of the embedded browser"),
    ("logs", "cache", false, "Application logs"),
    ("Network Persistent State", "cache", false, ""),
    ("Partitions", "cache", false, "Isolated storage for extensions/webviews"),
    ("process-monitor", "cache", false, ""),
    ("sentry", "cache", false, "Error reports sent to Sentry"),
    ("Workspaces", "sessions", false, "Record of open windows/workspaces"),
];

const CHAT_DB_FILES: &[&str] = &["state.vscdb", "state.vscdb.backup", "state.vscdb-wal", "state.vscdb-shm"];

fn sorted_children(dir: &Path) -> Vec<PathBuf> {
    let mut items: Vec<PathBuf> = fs::read_dir(dir).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    items.sort();
    items
}

fn file_name(path: &Path) -> String {
    path.file_name().unwrap_or_default().to_string_lossy().into_owned()
}

/// Project path from a workspaceStorage/<hash>/workspace.json ("folder" or
/// "workspace", as file://). Returns None if it is not readable.
fn read_workspace_path(ws_dir: &Path) -> Option<String> {
    let text = fs::read_to_string(ws_dir.join("workspace.json")).ok()?;
    let data: serde_json::Value = serde_json::from_str(&text).ok()?;
    let url = data
        .get("folder")
        .and_then(|v| v.as_str())
        .or_else(|| data.get("workspace").and_then(|v| v.as_str()))?;
    let rest = url.strip_prefix("file://")?;
    let path = percent_decode_str(rest).decode_utf8_lossy().into_owned();
    if path.is_empty() {
        None
    } else {
        Some(path)
    }
}

/// Number of conversations: one composerData:<id> row per chat. The query by
/// range uses the UNIQUE key index, so it does not scan the whole database.
fn count_conversations(db: &Path) -> Option<u64> {
    let con = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX).ok()?;
    con.query_row(
        "SELECT count(*) FROM cursorDiskKV WHERE key >= 'composerData:' AND key < 'composerData;'",
        [],
        |row| row.get::<_, u64>(0),
    )
    .ok()
}

fn chat_db_note(db_file: &Path) -> String {
    if file_name(db_file) != "state.vscdb" {
        return "Copy/journal of the Cursor Desktop chat database — not a cache, informational only".to_string();
    }
    let chats = match count_conversations(db_file) {
        Some(n) => format!("{n} conversations"),
        None => "conversations not readable right now".to_string(),
    };
    format!(
        "Cursor Desktop chat history ({chats}) plus UI state. \
         Not a cache: deleting it loses the conversations — informational only"
    )
}

fn scan_global_storage(gs: &Path, out: &mut Vec<Entry>) {
    for item in sorted_children(gs) {
        let name = file_name(&item);
        let Ok(meta) = fs::metadata(&item) else {
            continue;
        };
        if CHAT_DB_FILES.contains(&name.as_str()) {
            out.push(
                Entry::new(&item.to_string_lossy(), "cursor-desktop", "sessions", &item.to_string_lossy())
                    .with_stats(meta.len(), 1, mtime_secs(&meta))
                    .informational()
                    .note(chat_db_note(&item)),
            );
        } else if name.starts_with("conversation-search.db") {
            out.push(
                Entry::new(&item.to_string_lossy(), "cursor-desktop", "cache", &item.to_string_lossy())
                    .with_stats(meta.len(), 1, mtime_secs(&meta))
                    .note("Search index of the chat history, rebuilt automatically"),
            );
        } else if item.is_dir() {
            // One folder per extension: its own storage, regenerable.
            let (category, sensitive) = classify_by_name(&name);
            let category = if category == "other" { "cache" } else { category };
            let (size, count, mtime) = dir_stats(&item, &[]);
            out.push(
                Entry::new(&item.to_string_lossy(), "cursor-desktop", category, &item.to_string_lossy())
                    .with_stats(size, count, mtime)
                    .sensitive(sensitive)
                    .note("An extension's own storage"),
            );
        }
    }
}

fn scan_user_dir(user_dir: &Path, out: &mut Vec<Entry>) {
    let ws_storage = user_dir.join("workspaceStorage");
    if ws_storage.is_dir() {
        for ws in sorted_children(&ws_storage).into_iter().filter(|p| p.is_dir()) {
            let (size, count, mtime) = dir_stats(&ws, &[]);
            match read_workspace_path(&ws) {
                Some(project_path) => {
                    let orphaned = !Path::new(&project_path).exists();
                    let (category, note) = if orphaned {
                        ("orphaned", "This project no longer exists at that path")
                    } else {
                        ("sessions", "State of a project window (workspace)")
                    };
                    out.push(
                        Entry::new(&ws.to_string_lossy(), "cursor-desktop", category, &ws.to_string_lossy())
                            .with_stats(size, count, mtime)
                            .project(project_path.clone())
                            .title(project_path)
                            .note(note),
                    );
                }
                None => out.push(
                    Entry::new(&ws.to_string_lossy(), "cursor-desktop", "sessions", &ws.to_string_lossy())
                        .with_stats(size, count, mtime)
                        .note("Workspace without a readable workspace.json -- the project could not be identified"),
                ),
            }
        }
    }

    let history = user_dir.join("History");
    if history.is_dir() {
        let (size, count, mtime) = dir_stats(&history, &[]);
        out.push(
            Entry::new(&history.to_string_lossy(), "cursor-desktop", "cache", &history.to_string_lossy())
                .with_stats(size, count, mtime)
                .note("Local edit history (undo beyond the current session; not your only copy)"),
        );
    }

    let global_storage = user_dir.join("globalStorage");
    if global_storage.is_dir() {
        scan_global_storage(&global_storage, out);
    }

    // What remains in User/ (settings, keybindings, snippets) is grouped into one informational entry.
    let handled = ["workspaceStorage", "History", "globalStorage"];
    let (mut rest_size, mut rest_count, mut rest_mtime) = (0u64, 0u64, 0.0f64);
    for item in sorted_children(user_dir) {
        if handled.contains(&file_name(&item).as_str()) {
            continue;
        }
        let (s, c, m) = if item.is_dir() {
            dir_stats(&item, &[])
        } else {
            match fs::metadata(&item) {
                Ok(meta) => (meta.len(), 1, mtime_secs(&meta)),
                Err(_) => continue,
            }
        };
        rest_size += s;
        rest_count += c;
        rest_mtime = rest_mtime.max(m);
    }
    if rest_count > 0 {
        out.push(
            Entry::new(
                &format!("{}#config", user_dir.display()),
                "cursor-desktop",
                "config",
                &user_dir.to_string_lossy(),
            )
            .with_stats(rest_size, rest_count, rest_mtime)
            .informational()
            .note("User configuration (settings, keybindings, snippets) — informational only"),
        );
    }
}

pub fn scan(home: &Path) -> Vec<Entry> {
    let mut out = Vec::new();
    let root = home.join("Library").join("Application Support").join("Cursor");
    if !root.is_dir() {
        return out;
    }
    for child in sorted_children(&root).into_iter().filter(|p| p.is_dir()) {
        let name = file_name(&child);
        if name == "User" {
            scan_user_dir(&child, &mut out);
            continue;
        }
        let (category, sensitive, note) = match KNOWN_SUBDIRS.iter().find(|(n, ..)| *n == name) {
            Some((_, cat, sens, note)) => (cat.to_string(), *sens, note.to_string()),
            None => {
                let (cat, sens) = classify_by_name(&name);
                (cat.to_string(), sens, String::new())
            }
        };
        let (size, count, mtime) = dir_stats(&child, &[]);
        out.push(
            Entry::new(&child.to_string_lossy(), "cursor-desktop", &category, &child.to_string_lossy())
                .with_stats(size, count, mtime)
                .sensitive(sensitive)
                .note(note),
        );
    }
    out
}
