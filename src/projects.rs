//! Projects: which repository each session belongs to, and the actions that
//! follow from it (delete a session completely, move it to another project,
//! delete everything a project holds).
//!
//! Claude Code files a session under `~/.claude/projects/<folder>/`, where the
//! folder name is the project path with every character outside A-Z, a-z, 0-9
//! replaced by "-". The rule loses information, so the path cannot be recovered
//! from the name; it is read from the "cwd" recorded in the transcripts instead.
//!
//! A session is more than its transcript. Besides `<id>.jsonl` there can be a
//! folder `<id>/` next to it (sub-agents, tool results), `~/.claude/session-env/<id>`,
//! `~/.claude/file-history/<id>` and lines in `~/.claude/history.jsonl`. Deleting or
//! moving a session handles all of them. The real repository is never touched.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::UNIX_EPOCH;

use serde_json::{json, Value};

use crate::backup::{backup_file, log_change};
use crate::entry::Entry;
use crate::fsutil::{human_size, resolve_dashed_path};
use crate::trash;

/// Lines read from each transcript when looking for its working directory.
const CWD_SCAN_LINES: usize = 400;
/// Transcripts read per project folder when looking for its working directory.
const CWD_SCAN_FILES: usize = 8;

// ---------- Folder names ----------

/// Claude Code's folder name for a project path: every UTF-16 unit that is not
/// A-Z, a-z or 0-9 becomes "-" (as the JavaScript regex it uses does).
pub fn encode_folder(path: &str) -> String {
    path.encode_utf16()
        .map(|unit| match u8::try_from(unit) {
            Ok(byte) if byte.is_ascii_alphanumeric() => byte as char,
            _ => '-',
        })
        .collect()
}

pub fn projects_root(home: &Path) -> PathBuf {
    home.join(".claude").join("projects")
}

// ---------- Hints for folders whose path cannot be read ----------

/// Project paths the dashboard knows for a folder, kept for folders that hold no
/// transcript with a matching "cwd" (for example after a session was moved there).
fn hints() -> &'static Mutex<HashMap<String, String>> {
    static HINTS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    HINTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn hints_file(data_dir: &Path) -> PathBuf {
    data_dir.join("project_paths.json")
}

pub fn remember(folder_name: &str, path: &str) {
    if let Ok(mut map) = hints().lock() {
        map.insert(folder_name.to_string(), path.to_string());
    }
}

fn hint(folder_name: &str) -> Option<String> {
    hints().lock().ok()?.get(folder_name).cloned()
}

/// Loads the saved hints from the dashboard's data folder.
pub fn load_hints(data_dir: &Path) {
    let Ok(text) = fs::read_to_string(hints_file(data_dir)) else {
        return;
    };
    if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&text) {
        for (folder, path) in map {
            if let Some(path) = path.as_str() {
                remember(&folder, path);
            }
        }
    }
}

pub fn save_hints(data_dir: &Path) {
    let Ok(map) = hints().lock() else {
        return;
    };
    let _ = fs::create_dir_all(data_dir);
    let _ = fs::write(hints_file(data_dir), serde_json::to_string_pretty(&*map).unwrap_or_default());
}

// ---------- Project path of a Claude Code folder ----------

/// Distinct "cwd" values from the first lines of the transcripts in `folder`:
/// (the one whose encoding is the folder's name, the first one found).
fn scan_cwds(folder: &Path, folder_name: &str) -> (Option<String>, Option<String>) {
    let mut files: Vec<PathBuf> = fs::read_dir(folder)
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    files.retain(|p| p.extension().is_some_and(|e| e == "jsonl"));
    files.sort();

    let mut first: Option<String> = None;
    for file in files.into_iter().take(CWD_SCAN_FILES) {
        let Ok(handle) = fs::File::open(&file) else {
            continue;
        };
        for line in BufReader::new(handle).lines().take(CWD_SCAN_LINES).map_while(Result::ok) {
            if !line.contains("\"cwd\"") {
                continue;
            }
            let Ok(record) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let Some(cwd) = record.get("cwd").and_then(Value::as_str).filter(|c| !c.is_empty()) else {
                continue;
            };
            if encode_folder(cwd) == folder_name {
                return (Some(cwd.to_string()), first);
            }
            first.get_or_insert_with(|| cwd.to_string());
        }
    }
    (None, first)
}

/// (path, exists_on_disk) of the project a Claude Code folder stands for. In order:
/// a transcript "cwd" that encodes to the folder name, a saved hint, the first
/// "cwd" found, a path rebuilt from the name.
pub fn project_path_for_folder(folder: &Path) -> (String, bool) {
    let name = folder.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let (matching, first) = scan_cwds(folder, &name);
    if let Some(cwd) = matching.or_else(|| hint(&name)).or(first) {
        let exists = Path::new(&cwd).exists();
        return (cwd, exists);
    }
    if let Some(resolved) = resolve_dashed_path(&name, "/") {
        return (resolved, true);
    }
    (format!("/{}", name.trim_matches('-').replace('-', "/")), false)
}

// ---------- history.jsonl ----------

/// What to do with one line of history.jsonl.
pub enum HistoryAction {
    Keep,
    Drop,
    Changed,
}

/// Rewrites `~/.claude/history.jsonl` line by line. Does nothing, and makes no
/// backup, when no line changes. Lines that are not JSON are kept as they are.
/// Returns how many lines were dropped or changed.
fn rewrite_history(home: &Path, backups_dir: &Path, f: impl Fn(&mut Value) -> HistoryAction) -> Result<usize, String> {
    let path = home.join(".claude").join("history.jsonl");
    let Ok(meta) = fs::metadata(&path) else {
        return Ok(0);
    };
    let before = meta.modified().ok();
    let text = fs::read_to_string(&path).map_err(|e| format!("Could not read history.jsonl: {e}"))?;

    let mut out = String::with_capacity(text.len());
    let mut touched = 0usize;
    for line in text.lines() {
        match serde_json::from_str::<Value>(line) {
            Ok(mut record) => match f(&mut record) {
                HistoryAction::Keep => {
                    out.push_str(line);
                    out.push('\n');
                }
                HistoryAction::Drop => touched += 1,
                HistoryAction::Changed => {
                    touched += 1;
                    out.push_str(&serde_json::to_string(&record).unwrap_or_else(|_| line.to_string()));
                    out.push('\n');
                }
            },
            Err(_) => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    if touched == 0 {
        return Ok(0);
    }

    backup_file(&path, backups_dir, "claude-history.jsonl")?;
    // Claude Code appends to this file while it runs; refuse to overwrite a newer one.
    if fs::metadata(&path).ok().and_then(|m| m.modified().ok()) != before {
        return Err("history.jsonl changed while it was being rewritten; try again".into());
    }
    let tmp = path.with_extension("jsonl.dashboard-tmp");
    fs::write(&tmp, out).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(touched)
}

// ---------- A Claude Code session ----------

pub struct ClaudeSession {
    pub jsonl: PathBuf,
    pub folder: PathBuf,
    pub id: String,
}

fn is_session_id(text: &str) -> bool {
    text.len() == 36 && text.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// Checks that `path` is `~/.claude/projects/<folder>/<id>.jsonl`, nothing else.
pub fn claude_session(home: &Path, path: &str) -> Result<ClaudeSession, String> {
    let jsonl = PathBuf::from(path);
    if jsonl.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err("Not a Claude Code session".into());
    }
    let folder = jsonl.parent().ok_or("Not a Claude Code session")?.to_path_buf();
    if folder.parent() != Some(projects_root(home).as_path()) {
        return Err("Not a Claude Code session".into());
    }
    let id = match (jsonl.extension().and_then(|e| e.to_str()), jsonl.file_stem().and_then(|s| s.to_str())) {
        (Some("jsonl"), Some(stem)) if is_session_id(stem) => stem.to_string(),
        _ => return Err("Not a Claude Code session".into()),
    };
    if !jsonl.is_file() {
        return Err("The session no longer exists".into());
    }
    Ok(ClaudeSession { jsonl, folder, id })
}

impl ClaudeSession {
    /// Folders and files other than the transcript that belong to this session.
    fn companions(&self, home: &Path) -> Vec<PathBuf> {
        [
            self.folder.join(&self.id),
            home.join(".claude").join("session-env").join(&self.id),
            home.join(".claude").join("file-history").join(&self.id),
        ]
        .into_iter()
        .filter(|p| p.exists())
        .collect()
    }
}

/// Moves a session, with everything that belongs to it, to the dashboard trash, and
/// removes its lines from history.jsonl (after a backup). Nothing is deleted.
pub fn delete_claude_session(home: &Path, trash_dir: &Path, backups_dir: &Path, path: &str) -> Result<Value, String> {
    let session = claude_session(home, path)?;

    let moved = trash::move_to_trash(home, trash_dir, &session.jsonl.to_string_lossy());
    if moved["ok"] != json!(true) {
        return Err(moved["error"].as_str().unwrap_or("Could not move the session").to_string());
    }
    let mut count = 1;
    let mut warnings: Vec<String> = Vec::new();
    for extra in session.companions(home) {
        let result = trash::move_to_trash(home, trash_dir, &extra.to_string_lossy());
        if result["ok"] == json!(true) {
            count += 1;
        } else {
            warnings.push(format!("{}: {}", extra.display(), result["error"].as_str().unwrap_or("could not move")));
        }
    }

    let id = session.id.clone();
    let history_removed = match rewrite_history(home, backups_dir, |record| {
        if record.get("sessionId").and_then(Value::as_str) == Some(id.as_str()) {
            HistoryAction::Drop
        } else {
            HistoryAction::Keep
        }
    }) {
        Ok(n) => n,
        Err(error) => {
            warnings.push(error);
            0
        }
    };
    log_change(backups_dir, &format!("session {} deleted: {count} items moved to the trash, {history_removed} history lines", session.id));
    Ok(json!({"moved": count, "history_removed": history_removed, "warnings": warnings}))
}

/// Moves a session to another project: its transcript and folder go to the target's
/// folder and its history lines point to the target. The transcript is not edited;
/// Claude Code finds the session again by its id and keeps appending to the same file.
pub fn move_claude_session(home: &Path, backups_dir: &Path, path: &str, target: &str) -> Result<Value, String> {
    let session = claude_session(home, path)?;

    let target = target.trim_end_matches('/');
    let target_path = Path::new(target);
    let plain = target_path.is_absolute() && !target_path.components().any(|c| matches!(c, std::path::Component::ParentDir));
    if !plain || !target_path.starts_with(home) || !target_path.is_dir() {
        return Err("The target must be an existing folder inside your home folder".into());
    }
    let folder_name = encode_folder(target);
    let dest_folder = projects_root(home).join(&folder_name);
    if dest_folder == session.folder {
        return Err("The session already belongs to this project".into());
    }
    let dest_jsonl = dest_folder.join(format!("{}.jsonl", session.id));
    let sidecar = session.folder.join(&session.id);
    let dest_sidecar = dest_folder.join(&session.id);
    if dest_jsonl.exists() || dest_sidecar.exists() {
        return Err("The target project already has a session with this id".into());
    }

    fs::create_dir_all(&dest_folder).map_err(|e| format!("Could not create the project folder: {e}"))?;
    fs::rename(&session.jsonl, &dest_jsonl).map_err(|e| format!("Could not move the session: {e}"))?;
    if sidecar.is_dir() {
        if let Err(error) = fs::rename(&sidecar, &dest_sidecar) {
            // Put the transcript back so the session is not split between two projects.
            let _ = fs::rename(&dest_jsonl, &session.jsonl);
            return Err(format!("Could not move the session folder, nothing was changed: {error}"));
        }
    }
    remember(&folder_name, target);

    let id = session.id.clone();
    let target_owned = target.to_string();
    let mut warnings: Vec<String> = Vec::new();
    let history_changed = match rewrite_history(home, backups_dir, |record| {
        if record.get("sessionId").and_then(Value::as_str) != Some(id.as_str()) {
            return HistoryAction::Keep;
        }
        record["project"] = json!(target_owned);
        HistoryAction::Changed
    }) {
        Ok(n) => n,
        Err(error) => {
            warnings.push(error);
            0
        }
    };
    log_change(backups_dir, &format!("session {} moved to {target}: {history_changed} history lines updated", session.id));
    Ok(json!({"project": target, "history_updated": history_changed, "warnings": warnings}))
}

// ---------- A whole project ----------

/// Moves everything a Claude Code project folder holds to the trash, session by
/// session so each one is handled completely, then the folder itself (memory
/// included), and drops the project's remaining history lines.
fn delete_claude_project_folder(home: &Path, trash_dir: &Path, backups_dir: &Path, folder: &Path) -> Result<Value, String> {
    let name = folder.file_name().unwrap_or_default().to_string_lossy().into_owned();
    if folder.parent() != Some(projects_root(home).as_path()) || name.is_empty() {
        return Err("Not a Claude Code project folder".into());
    }
    let mut sessions = 0;
    let mut history_removed = 0u64;
    let mut warnings: Vec<String> = Vec::new();
    let mut transcripts: Vec<PathBuf> = fs::read_dir(folder)
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    transcripts.retain(|p| p.is_file() && p.extension().is_some_and(|e| e == "jsonl"));
    for jsonl in transcripts {
        match delete_claude_session(home, trash_dir, backups_dir, &jsonl.to_string_lossy()) {
            Ok(result) => {
                sessions += 1;
                history_removed += result["history_removed"].as_u64().unwrap_or(0);
                for w in result["warnings"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                    warnings.push(w.to_string());
                }
            }
            Err(error) => warnings.push(format!("{}: {error}", jsonl.display())),
        }
    }

    // Lines of this project whose transcript was already gone.
    history_removed += rewrite_history(home, backups_dir, |record| {
        match record.get("project").and_then(Value::as_str) {
            Some(project) if encode_folder(project) == name => HistoryAction::Drop,
            _ => HistoryAction::Keep,
        }
    })
    .unwrap_or_else(|error| {
        warnings.push(error);
        0
    }) as u64;

    let folder_result = trash::move_to_trash(home, trash_dir, &folder.to_string_lossy());
    if folder_result["ok"] != json!(true) {
        warnings.push(format!("project folder: {}", folder_result["error"].as_str().unwrap_or("could not move")));
    }
    Ok(json!({"sessions": sessions, "history_removed": history_removed, "warnings": warnings}))
}

/// Deletes everything the dashboard knows about `project` (a path from the scan):
/// Claude Code folders and their sessions, and the entries other tools keep for it.
/// Everything goes to the trash. The project's own folder on disk is never touched.
pub fn delete_project(home: &Path, trash_dir: &Path, backups_dir: &Path, project: &str, cursor_running: bool) -> Result<Value, String> {
    let scan = crate::scanner::run_scan(home);
    let entries: Vec<&Entry> = scan.entries.iter().filter(|e| e.project.as_deref() == Some(project)).collect();
    if entries.is_empty() {
        return Err("Unknown project".into());
    }

    let root = projects_root(home);
    let mut claude_folders: Vec<PathBuf> = Vec::new();
    let mut others: Vec<&Entry> = Vec::new();
    for entry in &entries {
        let parent = Path::new(&entry.path).parent().map(Path::to_path_buf);
        match parent {
            Some(folder) if entry.tool == "claude-code" && folder.parent() == Some(root.as_path()) => {
                if !claude_folders.contains(&folder) {
                    claude_folders.push(folder);
                }
            }
            _ => others.push(entry),
        }
    }

    let (mut sessions, mut history_removed, mut moved) = (0, 0, 0);
    let mut warnings: Vec<String> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    for folder in &claude_folders {
        match delete_claude_project_folder(home, trash_dir, backups_dir, folder) {
            Ok(result) => {
                sessions += result["sessions"].as_u64().unwrap_or(0);
                history_removed += result["history_removed"].as_u64().unwrap_or(0);
                for w in result["warnings"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                    warnings.push(w.to_string());
                }
            }
            Err(error) => warnings.push(error),
        }
    }
    for entry in others {
        if !entry.actionable {
            continue;
        }
        if entry.tool == "cursor-desktop" && cursor_running {
            skipped.push(json!({"path": entry.path, "reason": "Cursor is running: close it and try again"}));
            continue;
        }
        let result = trash::move_to_trash(home, trash_dir, &entry.path);
        if result["ok"] == json!(true) {
            moved += 1;
        } else {
            warnings.push(format!("{}: {}", entry.path, result["error"].as_str().unwrap_or("could not move")));
        }
    }
    log_change(backups_dir, &format!("project {project} deleted: {sessions} Claude Code sessions, {moved} other items, {history_removed} history lines"));
    Ok(json!({"project": project, "claude_sessions": sessions, "other_items": moved, "history_removed": history_removed, "skipped": skipped, "warnings": warnings}))
}

// ---------- Listing ----------

/// Projects with what the dashboard holds for each, newest activity first, and the
/// totals of entries that belong to no project.
pub fn list(entries: &[Entry]) -> Value {
    #[derive(Default)]
    struct Group {
        tools: Vec<String>,
        entries: u64,
        sessions: u64,
        size: u64,
        latest: f64,
    }
    let mut groups: HashMap<String, Group> = HashMap::new();
    let mut unassigned = Group::default();
    for e in entries {
        let g = match &e.project {
            Some(project) => groups.entry(project.clone()).or_default(),
            None => &mut unassigned,
        };
        if !g.tools.contains(&e.tool) {
            g.tools.push(e.tool.clone());
        }
        g.entries += 1;
        g.size += e.size_bytes;
        g.latest = g.latest.max(e.last_modified);
        if e.category == "sessions" {
            g.sessions += 1;
        }
    }
    let mut items: Vec<Value> = groups
        .into_iter()
        .map(|(project, g)| {
            json!({
                "project": project,
                "name": Path::new(&project).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| project.clone()),
                "exists": Path::new(&project).exists(),
                "tools": g.tools,
                "entries": g.entries,
                "sessions": g.sessions,
                "size_bytes": g.size,
                "size_human": human_size(g.size),
                "last_modified": g.latest,
            })
        })
        .collect();
    items.sort_by(|a, b| b["last_modified"].as_f64().partial_cmp(&a["last_modified"].as_f64()).unwrap_or(std::cmp::Ordering::Equal));
    json!({
        "items": items,
        "unassigned": {"entries": unassigned.entries, "size_bytes": unassigned.size, "size_human": human_size(unassigned.size)},
    })
}

// ---------- Plans ----------

/// The project a Claude Code plan was written in. Claude Code records the plan's
/// file name in the transcript of the session that wrote it, so the transcripts that
/// were open when the plan was saved are searched for that name. None when the
/// platform does not report creation times, or when nothing mentions the plan.
pub fn claude_plan_project(claude_root: &Path, plan: &Path) -> Option<String> {
    let name = plan.file_name()?.to_string_lossy().into_owned();
    let saved = crate::fsutil::mtime_secs(&fs::metadata(plan).ok()?);
    for folder in fs::read_dir(claude_root.join("projects")).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
        let Ok(files) = fs::read_dir(&folder) else {
            continue;
        };
        for file in files.flatten().map(|e| e.path()) {
            if !file.is_file() || !file.extension().is_some_and(|e| e == "jsonl") {
                continue;
            }
            let Ok(meta) = fs::metadata(&file) else {
                continue;
            };
            let Some(created) = meta.created().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs_f64()) else {
                continue;
            };
            // Only a transcript that existed when the plan was saved and kept going afterwards.
            if created > saved || crate::fsutil::mtime_secs(&meta) < saved {
                continue;
            }
            let Ok(handle) = fs::File::open(&file) else {
                continue;
            };
            if BufReader::new(handle).lines().map_while(Result::ok).any(|line| line.contains(&name)) {
                return Some(project_path_for_folder(&folder).0);
            }
        }
    }
    None
}
