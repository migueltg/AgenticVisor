//! Claude Desktop sessions.
//!
//! The desktop app keeps two kinds of session under
//! `~/Library/Application Support/Claude/`:
//! - code sessions, `claude-code-sessions/<account>/<org>/local_<id>.json`: only
//!   metadata (title, working directory, ...). The conversation itself is an ordinary
//!   Claude Code transcript in `~/.claude/projects/`, named by the JSON's
//!   "cliSessionId", so it is listed, grouped and deleted as a Claude Code session.
//!   The JSON goes with it when that session is deleted.
//! - agent mode sessions, `local-agent-mode-sessions/<account>/<org>/local_<id>.json`
//!   plus a folder `local_<id>/` with its own files. These are listed one by one. Their
//!   "cwd" is a folder inside the app, so the project is the first of the folders the
//!   user selected, when there is one.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::backup::log_change;
use crate::entry::Entry;
use crate::fsutil::{dir_stats, mtime_secs};
use crate::projects::is_session_id;
use crate::titles::clean_title;
use crate::trash;

pub fn app_root(home: &Path) -> PathBuf {
    home.join("Library").join("Application Support").join("Claude")
}

fn children(dir: &Path) -> Vec<PathBuf> {
    let mut items: Vec<PathBuf> = fs::read_dir(dir).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    items.sort();
    items
}

/// Every `local_<id>.json` two levels below `base` (`<account>/<org>/`).
fn session_jsons(base: &Path) -> Vec<PathBuf> {
    children(base)
        .into_iter()
        .filter(|p| p.is_dir())
        .flat_map(|account| children(&account).into_iter().filter(|p| p.is_dir()))
        .flat_map(|org| children(&org))
        .filter(|p| {
            p.is_file()
                && p.extension().is_some_and(|e| e == "json")
                && p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("local_"))
        })
        .collect()
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

fn normalize(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() { "/".to_string() } else { trimmed.to_string() }
}

/// The project of a code session: the folder it was started from ("originCwd", which
/// is the repository even when the session ran in a worktree), else its "cwd". A
/// folder inside the app itself is a scratch space, not a project.
fn code_session_project(home: &Path, json: &Value) -> Option<String> {
    let cwd = json.get("originCwd").and_then(Value::as_str).or_else(|| json.get("cwd").and_then(Value::as_str))?;
    let cwd = normalize(cwd);
    (!cwd.is_empty() && !Path::new(&cwd).starts_with(app_root(home))).then_some(cwd)
}

/// Metadata files of code sessions whose conversation is the Claude Code session `id`.
pub fn metadata_for_cli_session(home: &Path, id: &str) -> Vec<PathBuf> {
    session_jsons(&app_root(home).join("claude-code-sessions"))
        .into_iter()
        .filter(|p| read_json(p).is_some_and(|j| j.get("cliSessionId").and_then(Value::as_str) == Some(id)))
        .collect()
}

/// Metadata files of the code sessions that belong to `project`.
pub fn code_session_jsons_for_project(home: &Path, project: &str) -> Vec<PathBuf> {
    session_jsons(&app_root(home).join("claude-code-sessions"))
        .into_iter()
        .filter(|p| read_json(p).and_then(|j| code_session_project(home, &j)).as_deref() == Some(project))
        .collect()
}

fn seconds(value: Option<&Value>) -> f64 {
    let n = value.and_then(Value::as_f64).unwrap_or(0.0);
    if n > 1e11 { n / 1000.0 } else { n }
}

/// One entry per agent mode session under `dir` (`local-agent-mode-sessions`).
pub fn agent_mode_entries(dir: &Path) -> Vec<Entry> {
    let mut out = Vec::new();
    for json_path in session_jsons(dir) {
        let Some(stem) = json_path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
            continue;
        };
        let folder = json_path.with_file_name(&stem);
        let (mut size, mut count, mut mtime) = (0, 0, 0.0);
        if folder.is_dir() {
            (size, count, mtime) = dir_stats(&folder, &[]);
        }
        if let Ok(meta) = fs::metadata(&json_path) {
            size += meta.len();
            count += 1;
            mtime = f64::max(mtime, mtime_secs(&meta));
        }
        let json = read_json(&json_path).unwrap_or(Value::Null);
        let title = json.get("title").and_then(Value::as_str).filter(|t| !t.is_empty()).map(clean_title).unwrap_or_else(|| stem.clone());
        let id = if folder.is_dir() { folder.clone() } else { json_path.clone() };
        let mut entry = Entry::new(&id.to_string_lossy(), "claude-desktop", "sessions", &id.to_string_lossy())
            .with_stats(size, count, mtime)
            .title(title)
            .note("Claude Desktop agent mode session");
        entry.created = seconds(json.get("createdAt"));
        let selected = json.get("userSelectedFolders").and_then(Value::as_array).and_then(|a| a.first()).and_then(Value::as_str);
        if let Some(project) = selected.map(normalize).filter(|p| !p.is_empty()) {
            entry.note = format!("Claude Desktop agent mode session · Project: {project}");
            entry.project = Some(project);
        }
        out.push(entry);
    }
    out
}

/// (folder, metadata file) of an agent mode session, if `path` is one of its two
/// parts: `local-agent-mode-sessions/<account>/<org>/local_<id>` or the same plus ".json".
pub fn agent_session(home: &Path, path: &str) -> Option<(PathBuf, PathBuf)> {
    let p = PathBuf::from(path);
    if p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return None;
    }
    let folder = if p.extension().is_some_and(|e| e == "json") { p.with_extension("") } else { p };
    let name = folder.file_name()?.to_str()?;
    if !name.strip_prefix("local_").is_some_and(is_session_id) {
        return None;
    }
    let relative = folder.strip_prefix(app_root(home).join("local-agent-mode-sessions")).ok()?;
    (relative.components().count() == 3).then(|| (folder.clone(), folder.with_extension("json")))
}

/// Moves an agent mode session, its folder and its metadata file, to the trash.
pub fn delete_agent_mode_session(home: &Path, trash_dir: &Path, backups_dir: &Path, path: &str) -> Result<Value, String> {
    let (folder, json_path) = agent_session(home, path).ok_or("Not a Claude Desktop agent mode session")?;
    if !folder.exists() && !json_path.exists() {
        return Err("The session no longer exists".into());
    }
    let mut moved = 0;
    let mut warnings: Vec<String> = Vec::new();
    for part in [&folder, &json_path] {
        if !part.exists() {
            continue;
        }
        let result = trash::move_to_trash(home, trash_dir, &part.to_string_lossy());
        if result["ok"] == json!(true) {
            moved += 1;
        } else {
            warnings.push(format!("{}: {}", part.display(), result["error"].as_str().unwrap_or("could not move")));
        }
    }
    if moved == 0 {
        return Err(warnings.join("; "));
    }
    log_change(backups_dir, &format!("claude desktop agent mode session {} deleted: {moved} items moved to the trash", folder.display()));
    Ok(json!({"moved": moved, "warnings": warnings}))
}
