//! Inventory of the backups the dashboard makes before each write (copies of
//! settings, skills and configuration files). Backups are only listed and moved to
//! the dashboard trash, never deleted directly.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::fsutil::{created_secs, human_size, mtime_secs};
use crate::trash;

/// Backups in `backups_dir`, newest first. Names are "<stamp>__<label>".
pub fn list(backups_dir: &Path) -> Value {
    let mut items: Vec<Value> = Vec::new();
    let mut total = 0u64;
    for entry in fs::read_dir(backups_dir).into_iter().flatten().flatten() {
        let path = entry.path();
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let label = name.split_once("__").map(|(_, label)| label.to_string()).unwrap_or_default();
        total += meta.len();
        items.push(json!({
            "name": name,
            "path": path.to_string_lossy(),
            "label": label,
            "size_bytes": meta.len(),
            "size_human": human_size(meta.len()),
            "created": created_secs(&path),
            "last_modified": mtime_secs(&meta),
        }));
    }
    items.sort_by(|a, b| b["name"].as_str().cmp(&a["name"].as_str()));
    json!({"items": items, "count": items.len(), "total_size_bytes": total, "total_size_human": human_size(total)})
}

/// Path of a backup named `name`. Rejects anything that is not a plain file name
/// inside `backups_dir`.
pub fn backup_path(backups_dir: &Path, name: &str) -> Result<PathBuf, String> {
    let plain = !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\']);
    if !plain {
        return Err(format!("Not a backup name: {name}"));
    }
    let path = backups_dir.join(name);
    if !path.is_file() {
        return Err(format!("No such backup: {name}"));
    }
    Ok(path)
}

/// Moves the named backups to the dashboard trash, one result per name.
pub fn move_to_trash(home: &Path, backups_dir: &Path, trash_dir: &Path, names: &[String]) -> Value {
    let mut results = serde_json::Map::new();
    for name in names {
        let result = match backup_path(backups_dir, name) {
            Ok(path) => trash::move_to_trash(home, trash_dir, &path.to_string_lossy()),
            Err(error) => json!({"ok": false, "error": error}),
        };
        results.insert(name.clone(), result);
    }
    json!({"ok": true, "results": results})
}
