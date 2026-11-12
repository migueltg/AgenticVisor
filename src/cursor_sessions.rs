//! Cursor conversations: which project they belong to, deleting one completely,
//! and moving one to another project.
//!
//! Cursor keeps a conversation in several places, by its id:
//! - `~/.cursor/chats/<md5 of the project path>/<id>/` (meta.json holds the "cwd",
//!   store.db the messages), written by the command line agent;
//! - `~/.cursor/projects/<slug>/agent-transcripts/<id>/`, where the slug is the
//!   project path with every character outside A-Z, a-z, 0-9 replaced by "-" and
//!   no leading "-";
//! - rows `composerData:<id>` and `bubbleId:<id>:*` in the IDE's global database,
//!   and the id in the chat list of each workspace database that shows it.
//!
//! A conversation made with the command line agent has no IDE rows and can be moved
//! to another project. One made in the IDE is tied to the IDE's own lists, which
//! can only be edited with Cursor closed and cannot be checked from here, so it is
//! deleted but not moved.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OpenFlags};
use serde_json::{json, Value};

use crate::backup::log_change;
use crate::cursor_chats::{chat_database, delete_chat};
use crate::entry::Entry;
use crate::fsutil::sql_bytes;
use crate::projects::{encode_folder, is_session_id, projects_root, remember};
use crate::titles::clean_title;
use crate::trash;

fn cursor_root(home: &Path) -> PathBuf {
    home.join(".cursor")
}

fn workspace_storage(home: &Path) -> PathBuf {
    home.join("Library").join("Application Support").join("Cursor").join("User").join("workspaceStorage")
}

/// Cursor's folder name for a project path.
pub fn slug_of(path: &str) -> String {
    encode_folder(path).trim_start_matches('-').to_string()
}

/// Name of the folder under `~/.cursor/chats/` for a project path.
pub fn chats_folder_of(path: &str) -> String {
    format!("{:x}", md5::compute(path.as_bytes()))
}

fn normalize(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() { "/".to_string() } else { trimmed.to_string() }
}

// ---------- Known project paths ----------

/// Cursor folder name -> project path, for every path the dashboard can read from
/// somewhere: Claude Code transcripts, Cursor workspaces and Cursor chat metadata.
/// Only paths that exist on disk are kept. Used to turn a slug back into a path,
/// which the naming rule alone cannot do.
pub fn known_paths(home: &Path) -> HashMap<String, String> {
    let mut paths: Vec<String> = Vec::new();

    if let Ok(read) = fs::read_dir(projects_root(home)) {
        for folder in read.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
            paths.push(crate::projects::project_path_for_folder(&folder).0);
        }
    }
    if let Ok(read) = fs::read_dir(workspace_storage(home)) {
        for ws in read.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
            if let Some(path) = crate::adapters::cursor_desktop::read_workspace_path(&ws) {
                paths.push(path);
            }
        }
    }
    for meta in chat_dirs(home).into_iter().map(|d| d.join("meta.json")) {
        if let Some(cwd) = read_meta_cwd(&meta) {
            paths.push(cwd);
        }
    }

    paths
        .into_iter()
        .map(|p| normalize(&p))
        .filter(|p| Path::new(p).exists())
        .map(|p| (slug_of(&p), p))
        .collect()
}

/// Project path for a slug: a known path, a path saved after a move, or None.
pub fn project_for_slug(known: &HashMap<String, String>, slug: &str) -> Option<String> {
    known.get(slug).cloned().or_else(|| crate::projects::hint(slug).filter(|p| Path::new(p).exists()))
}

fn read_meta_cwd(meta: &Path) -> Option<String> {
    let value: Value = serde_json::from_str(&fs::read_to_string(meta).ok()?).ok()?;
    value.get("cwd").and_then(Value::as_str).filter(|c| !c.is_empty()).map(str::to_string)
}

fn children(dir: &Path) -> Vec<PathBuf> {
    let mut items: Vec<PathBuf> = fs::read_dir(dir).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    items.sort();
    items
}

/// Every `~/.cursor/chats/<md5>/<id>/` folder.
pub fn chat_dirs(home: &Path) -> Vec<PathBuf> {
    children(&cursor_root(home).join("chats"))
        .into_iter()
        .filter(|p| p.is_dir())
        .flat_map(|hash| children(&hash).into_iter().filter(|p| p.is_dir()))
        .collect()
}

// ---------- Finding a conversation ----------

pub struct Conversation {
    pub id: String,
    pub transcripts: Vec<PathBuf>,
    pub chats: Vec<PathBuf>,
}

pub fn find_conversation(home: &Path, id: &str) -> Conversation {
    let transcripts = children(&cursor_root(home).join("projects"))
        .into_iter()
        .map(|slug| slug.join("agent-transcripts").join(id))
        .filter(|p| p.is_dir())
        .collect();
    let chats = children(&cursor_root(home).join("chats"))
        .into_iter()
        .map(|hash| hash.join(id))
        .filter(|p| p.is_dir())
        .collect();
    Conversation { id: id.to_string(), transcripts, chats }
}

/// The conversation id a path or session key stands for, if it is one of Cursor's
/// conversation locations: a transcript folder, a chat folder, or the IDE key
/// "<global database>#<id>".
pub fn conversation_id_of_path(home: &Path, path: &str) -> Option<String> {
    if let Some((db, id)) = path.rsplit_once('#') {
        return (Path::new(db) == chat_database(home) && is_session_id(id)).then(|| id.to_string());
    }
    let p = PathBuf::from(path);
    if p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return None;
    }
    let id = p.file_name()?.to_str()?.to_string();
    if !is_session_id(&id) {
        return None;
    }
    let parent = p.parent()?;
    let cursor = cursor_root(home);
    let in_transcripts = parent.file_name().is_some_and(|n| n == "agent-transcripts")
        && parent.parent().and_then(Path::parent) == Some(cursor.join("projects").as_path());
    let in_chats = parent.parent() == Some(cursor.join("chats").as_path());
    (in_transcripts || in_chats).then_some(id)
}

fn global_has_chat(home: &Path, id: &str) -> bool {
    let db = chat_database(home);
    let Ok(con) = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX) else {
        return false;
    };
    con.query_row("SELECT 1 FROM cursorDiskKV WHERE key = ?1", params![format!("composerData:{id}")], |_| Ok(()))
        .is_ok()
}

fn workspace_dbs(home: &Path) -> Vec<PathBuf> {
    children(&workspace_storage(home)).into_iter().map(|ws| ws.join("state.vscdb")).filter(|p| p.is_file()).collect()
}

/// The chat list a workspace keeps (`composer.composerData`), as JSON.
fn read_list(con: &Connection) -> Option<Value> {
    let raw = con
        .query_row("SELECT value FROM ItemTable WHERE key = 'composer.composerData'", [], |row| row.get::<_, rusqlite::types::Value>(0))
        .ok()?;
    serde_json::from_slice(&sql_bytes(raw)?).ok()
}

fn list_mentions(list: &Value, id: &str) -> bool {
    let in_all = list.get("allComposers").and_then(Value::as_array).is_some_and(|a| a.iter().any(|c| c["composerId"] == id));
    let in_selected = list.get("selectedComposerIds").and_then(Value::as_array).is_some_and(|a| a.iter().any(|c| c == id));
    in_all || in_selected
}

fn workspaces_listing(home: &Path, id: &str) -> Vec<PathBuf> {
    workspace_dbs(home)
        .into_iter()
        .filter(|db| {
            Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
                .ok()
                .and_then(|con| read_list(&con))
                .is_some_and(|list| list_mentions(&list, id))
        })
        .collect()
}

/// Removes the id from one workspace's chat list, after a full copy of its database.
fn remove_from_workspace_list(db: &Path, backups_dir: &Path, id: &str) -> Result<(), String> {
    let con = Connection::open(db).map_err(|e| format!("Could not open a workspace database: {e}"))?;
    let Some(mut list) = read_list(&con) else {
        return Ok(());
    };
    if !list_mentions(&list, id) {
        return Ok(());
    }
    fs::create_dir_all(backups_dir).map_err(|e| e.to_string())?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let hash = db.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let backup = backups_dir.join(format!("{stamp}__cursor-workspace-{hash}-{id}.vscdb"));
    con.execute("VACUUM INTO ?1", params![backup.to_string_lossy()])
        .map_err(|e| format!("Could not back up a workspace database, nothing was changed: {e}"))?;

    if let Some(all) = list.get_mut("allComposers").and_then(Value::as_array_mut) {
        all.retain(|c| c["composerId"] != id);
    }
    if let Some(selected) = list.get_mut("selectedComposerIds").and_then(Value::as_array_mut) {
        selected.retain(|c| c != id);
    }
    con.execute(
        "UPDATE ItemTable SET value = ?1 WHERE key = 'composer.composerData'",
        params![serde_json::to_string(&list).map_err(|e| e.to_string())?],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// A chat's folder under `chats/<md5>/` is left empty once its last chat has gone; remove it.
/// `remove_dir` only succeeds on an empty folder, so nothing else can be lost.
fn remove_empty_chat_parents(conversation: &Conversation) {
    for chat in &conversation.chats {
        if let Some(parent) = chat.parent() {
            let _ = fs::remove_dir(parent);
        }
    }
}

// ---------- Deleting a conversation ----------

/// Deletes a conversation with everything that belongs to it: its IDE rows and its
/// place in the workspace chat lists (each after a backup of the database), and its
/// transcript and chat folders, which go to the trash. Refused while Cursor runs if
/// the IDE holds any part of it, because the IDE keeps its databases open.
pub fn delete_conversation(home: &Path, trash_dir: &Path, backups_dir: &Path, id: &str, cursor_running: bool) -> Result<Value, String> {
    if !is_session_id(id) {
        return Err("Not a conversation identifier".into());
    }
    let conversation = find_conversation(home, id);
    let in_ide = global_has_chat(home, id);
    let listing = workspaces_listing(home, id);
    if conversation.transcripts.is_empty() && conversation.chats.is_empty() && !in_ide && listing.is_empty() {
        return Err("The conversation no longer exists".into());
    }
    if (in_ide || !listing.is_empty()) && cursor_running {
        return Err("Cursor is running: close it before deleting a conversation.".into());
    }

    let mut warnings: Vec<String> = Vec::new();
    let mut ide_rows = 0;
    if in_ide {
        let key = format!("{}#{id}", chat_database(home).display());
        ide_rows = delete_chat(home, backups_dir, &key, false)?["deleted_rows"].as_u64().unwrap_or(0);
    }
    let mut lists_updated = 0;
    for db in &listing {
        match remove_from_workspace_list(db, backups_dir, id) {
            Ok(()) => lists_updated += 1,
            Err(error) => warnings.push(error),
        }
    }
    let mut moved = 0;
    for folder in conversation.transcripts.iter().chain(conversation.chats.iter()) {
        let result = trash::move_to_trash(home, trash_dir, &folder.to_string_lossy());
        if result["ok"] == json!(true) {
            moved += 1;
        } else {
            warnings.push(format!("{}: {}", folder.display(), result["error"].as_str().unwrap_or("could not move")));
        }
    }
    remove_empty_chat_parents(&conversation);
    log_change(backups_dir, &format!("cursor conversation {id} deleted: {ide_rows} IDE rows, {lists_updated} workspace lists, {moved} folders moved to the trash"));
    Ok(json!({"deleted_rows": ide_rows, "workspace_lists": lists_updated, "moved": moved, "warnings": warnings}))
}

// ---------- Moving a conversation ----------

/// Moves a conversation made with the command line agent to another project: its chat
/// folder goes under the target's hash and its "cwd" is updated, and its transcript
/// folder goes to the target's project folder. The conversation keeps its id and
/// `cursor-agent --resume` finds it again from the new project.
pub fn move_conversation(home: &Path, path: &str, target: &str) -> Result<Value, String> {
    let id = conversation_id_of_path(home, path).ok_or("Not a Cursor conversation")?;
    if global_has_chat(home, &id) || !workspaces_listing(home, &id).is_empty() {
        return Err("This conversation was made in the Cursor IDE. Its lists live in the IDE's databases, so it cannot be moved from here.".into());
    }
    let conversation = find_conversation(home, &id);
    if conversation.chats.is_empty() && conversation.transcripts.is_empty() {
        return Err("The conversation no longer exists".into());
    }

    let target = normalize(target);
    let target_path = Path::new(&target);
    let plain = target_path.is_absolute() && !target_path.components().any(|c| matches!(c, std::path::Component::ParentDir));
    if !plain || !target_path.starts_with(home) || !target_path.is_dir() {
        return Err("The target must be an existing folder inside your home folder".into());
    }

    let chats_dest = cursor_root(home).join("chats").join(chats_folder_of(&target));
    let slug = slug_of(&target);
    let transcripts_dest = cursor_root(home).join("projects").join(&slug).join("agent-transcripts");

    // Plan every rename first, so a conflict is found before anything moves.
    let mut renames: Vec<(PathBuf, PathBuf)> = Vec::new();
    for chat in &conversation.chats {
        renames.push((chat.clone(), chats_dest.join(&id)));
    }
    for transcript in &conversation.transcripts {
        renames.push((transcript.clone(), transcripts_dest.join(&id)));
    }
    renames.retain(|(from, to)| from != to);
    if renames.is_empty() {
        return Err("The conversation already belongs to this project".into());
    }
    if renames.iter().any(|(_, to)| to.exists()) {
        return Err("The target project already has a conversation with this id".into());
    }

    let mut done: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (from, to) in &renames {
        let step = fs::create_dir_all(to.parent().unwrap_or(Path::new("."))).and_then(|_| fs::rename(from, to));
        if let Err(error) = step {
            for (original, moved) in done.iter().rev() {
                let _ = fs::rename(moved, original);
            }
            return Err(format!("Could not move the conversation, nothing was changed: {error}"));
        }
        done.push((from.clone(), to.clone()));
    }
    // The chat's metadata names its project as well as the folder does.
    for chat in &conversation.chats {
        let meta = chats_dest.join(chat.file_name().unwrap_or_default()).join("meta.json");
        if let Ok(text) = fs::read_to_string(&meta) {
            if let Ok(mut value) = serde_json::from_str::<Value>(&text) {
                value["cwd"] = json!(target);
                let _ = fs::write(&meta, serde_json::to_string(&value).unwrap_or(text));
            }
        }
    }
    remove_empty_chat_parents(&conversation);
    remember(&slug, &target);
    Ok(json!({"project": target, "moved": done.len(), "warnings": []}))
}

// ---------- Entries for IDE chats ----------

fn as_f64(value: &rusqlite::types::Value) -> f64 {
    match value {
        rusqlite::types::Value::Integer(i) => *i as f64,
        rusqlite::types::Value::Real(r) => *r,
        rusqlite::types::Value::Text(t) => t.parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// One entry per IDE chat that a workspace lists and that has no transcript folder
/// (the ones with a transcript already appear through it), with the workspace's
/// folder as its project. The path is the key the preview and delete use.
pub fn ide_chat_entries(home: &Path) -> Vec<Entry> {
    let global = chat_database(home);
    let Ok(con) = Connection::open_with_flags(&global, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX) else {
        return Vec::new();
    };
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for ws in children(&workspace_storage(home)).into_iter().filter(|p| p.is_dir()) {
        let Some(project) = crate::adapters::cursor_desktop::read_workspace_path(&ws).map(|p| normalize(&p)) else {
            continue;
        };
        let Ok(ws_con) = Connection::open_with_flags(ws.join("state.vscdb"), OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX) else {
            continue;
        };
        let Some(list) = read_list(&ws_con) else {
            continue;
        };
        for id in list.get("allComposers").and_then(Value::as_array).into_iter().flatten().filter_map(|c| c["composerId"].as_str()) {
            if seen.iter().any(|s| s == id) || !find_conversation(home, id).transcripts.is_empty() {
                continue;
            }
            let row = con.query_row(
                "SELECT json_extract(CAST(value AS TEXT), '$.name'), json_extract(CAST(value AS TEXT), '$.lastUpdatedAt'), \
                 json_extract(CAST(value AS TEXT), '$.createdAt'), length(value) FROM cursorDiskKV WHERE key = ?1",
                params![format!("composerData:{id}")],
                |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, rusqlite::types::Value>(1)?, r.get::<_, rusqlite::types::Value>(2)?, r.get::<_, i64>(3)?)),
            );
            let Ok((name, updated, created, length)) = row else {
                continue;
            };
            seen.push(id.to_string());
            let key = format!("{}#{id}", global.display());
            let title = name.filter(|n| !n.is_empty()).map(|n| clean_title(&n)).unwrap_or_else(|| "(untitled)".to_string());
            let mut entry = Entry::new(&key, "cursor-desktop", "sessions", &key)
                .with_stats(length.max(0) as u64, 1, as_f64(&updated) / 1000.0)
                .title(title)
                .project(project.clone())
                .note(format!("Cursor IDE chat · Project: {project}"));
            entry.created = as_f64(&created) / 1000.0;
            out.push(entry);
        }
    }
    out
}
