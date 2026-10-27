//! Deletes one conversation from the Cursor Desktop chat database.
//!
//! The database holds all chats, so only the rows of the chosen conversation are
//! removed. Before anything is deleted, a consistent copy of the whole database is
//! written to the backups folder. Deletion is refused while Cursor is running,
//! because the app keeps the database open and would overwrite the change.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::Local;
use rusqlite::{params, Connection, OpenFlags};
use serde_json::{json, Value};

use crate::backup::log_change;

/// Path of Cursor Desktop's chat database inside `home`.
pub fn chat_database(home: &Path) -> PathBuf {
    home.join("Library").join("Application Support").join("Cursor").join("User").join("globalStorage").join("state.vscdb")
}

/// Splits a session key "<database>#<conversation id>" as the search index builds it.
fn split_key(key: &str) -> Option<(&str, &str)> {
    key.rsplit_once('#')
}

fn is_conversation_id(id: &str) -> bool {
    id.len() == 36 && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// Deletes the conversation identified by `key`. `cursor_running` is passed in so
/// the caller decides how to detect it and tests can set it.
pub fn delete_chat(home: &Path, backups_dir: &Path, key: &str, cursor_running: bool) -> Result<Value, String> {
    let (db_text, conversation) = split_key(key).ok_or("Not a Cursor conversation key")?;
    let db = chat_database(home);
    if Path::new(db_text) != db {
        return Err("This conversation is not in the Cursor chat database of this home".into());
    }
    if !is_conversation_id(conversation) {
        return Err("Not a conversation identifier".into());
    }
    if cursor_running {
        return Err("Cursor is running: close it before deleting a conversation.".into());
    }
    if !db.is_file() {
        return Err("The Cursor chat database does not exist".into());
    }

    let backup = write_backup(&db, backups_dir, conversation)?;
    let deleted = delete_rows(&db, conversation)?;
    log_change(backups_dir, &format!("cursor conversation {conversation} deleted: {deleted} rows; backup {}", backup.display()));

    Ok(json!({"deleted_rows": deleted, "backup": backup.to_string_lossy()}))
}

/// Writes a consistent copy of the database with VACUUM INTO, so the copy includes
/// anything still in the write-ahead log.
fn write_backup(db: &Path, backups_dir: &Path, conversation: &str) -> Result<PathBuf, String> {
    fs::create_dir_all(backups_dir).map_err(|e| format!("Could not create the backups folder: {e}"))?;
    let stamp = Local::now().format("%Y%m%d-%H%M%S");
    let dest = backups_dir.join(format!("{stamp}__cursor-state-{conversation}.vscdb"));
    let con = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|e| format!("Could not open the chat database: {e}"))?;
    con.execute("VACUUM INTO ?1", params![dest.to_string_lossy()])
        .map_err(|e| format!("Could not create the database backup, nothing was deleted: {e}"))?;
    Ok(dest)
}

/// Removes the conversation's metadata row and its message rows in one transaction.
fn delete_rows(db: &Path, conversation: &str) -> Result<usize, String> {
    let mut con = Connection::open(db).map_err(|e| format!("Could not open the chat database: {e}"))?;
    let tx = con.transaction().map_err(|e| e.to_string())?;
    let mut deleted = tx
        .execute("DELETE FROM cursorDiskKV WHERE key = ?1", params![format!("composerData:{conversation}")])
        .map_err(|e| e.to_string())?;
    // Message rows are keyed "bubbleId:<conversation>:<message>"; the range matches only that prefix.
    deleted += tx
        .execute(
            "DELETE FROM cursorDiskKV WHERE key >= ?1 AND key < ?2",
            params![format!("bubbleId:{conversation}:"), format!("bubbleId:{conversation};")],
        )
        .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(deleted)
}
