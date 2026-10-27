//! Filesystem utilities shared by all adapters:
//! folder sizes, classification by name, and reading the frontmatter of
//! skills.

use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

use regex::Regex;

/// Names that Electron/Chromium regenerates by itself: always "cache".
const CACHE_DIR_NAMES: &[&str] = &[
    "Cache", "Code Cache", "GPUCache", "DawnCache", "DawnGraphiteCache",
    "DawnWebGPUCache", "blob_storage", "IndexedDB", "Local Storage",
    "Session Storage", "Service Worker", "WebStorage", "SharedStorage",
    "Crashpad", "VideoDecodeStats", "ShaderCache", "CachedData", "fcache",
    "shared_proto_db", "Shared Dictionary", "Partitions",
];

/// Name fragments that reveal configuration or credentials.
const SENSITIVE_NAME_HINTS: &[&str] = &["token", "auth", "cookie", "credential", "secret", "mcp", "key"];

const SESSION_NAME_HINTS: &[&str] = &[
    "session", "chat", "transcript", "history", "project", "conversation", "workspace",
];

/// Human-readable size: bytes without decimals, the rest with one decimal.
pub fn human_size(n: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut size = n as f64;
    for unit in units {
        if size < 1024.0 {
            return if unit == "B" {
                format!("{}{}", size as u64, unit)
            } else {
                format!("{:.1}{}", size, unit)
            };
        }
        size /= 1024.0;
    }
    format!("{:.1}PB", size)
}

/// Seconds since the epoch of the last modification; 0.0 if unknown.
pub fn mtime_secs(meta: &fs::Metadata) -> f64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Seconds since the epoch when `path` was created; 0.0 where the platform does not record it.
pub fn created_secs(path: &Path) -> f64 {
    fs::metadata(path)
        .ok()
        .and_then(|m| m.created().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Walks `path` once: (total size, number of files, most recent mtime).
/// Does not follow symlinks (avoids loops and double counting). `skip_children` excludes
/// direct subfolders by name, when they are listed as their own entries.
pub fn dir_stats(path: &Path, skip_children: &[&str]) -> (u64, u64, f64) {
    let mut total_size = 0u64;
    let mut file_count = 0u64;
    let mut latest = 0.0f64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                let name = entry.file_name();
                if current == path && skip_children.contains(&name.to_string_lossy().as_ref()) {
                    continue;
                }
                stack.push(entry.path());
            } else if let Ok(meta) = entry.metadata() {
                total_size += meta.len();
                file_count += 1;
                latest = latest.max(mtime_secs(&meta));
            }
        }
    }
    (total_size, file_count, latest)
}

/// Category and sensitivity from the name, when no adapter knows better.
pub fn classify_by_name(name: &str) -> (&'static str, bool) {
    let lname = name.to_lowercase();
    if CACHE_DIR_NAMES.contains(&name) || lname.contains("cache") {
        return ("cache", false);
    }
    if SENSITIVE_NAME_HINTS.iter().any(|h| lname.contains(h)) {
        return ("config", true);
    }
    if SESSION_NAME_HINTS.iter().any(|h| lname.contains(h)) {
        return ("sessions", false);
    }
    ("other", false)
}

/// Converts a folder name where "/" was replaced by "-" into a real path,
/// checking the disk at each step. Prefers the longest name and also tries
/// the name with a leading dot (".hermes" appears as "hermes").
pub fn resolve_dashed_path(folder_name: &str, base: &str) -> Option<String> {
    let parts: Vec<&str> = folder_name.trim_matches('-').split('-').collect();
    walk(base, &parts)
}

fn walk(current: &str, remaining: &[&str]) -> Option<String> {
    if remaining.is_empty() {
        return Some(current.to_string());
    }
    for end in (1..=remaining.len()).rev() {
        let name = remaining[..end].join("-");
        for candidate_name in [name.clone(), format!(".{name}")] {
            let candidate = Path::new(current).join(&candidate_name);
            if candidate.exists() {
                if let Some(found) = walk(&candidate.to_string_lossy(), &remaining[end..]) {
                    return Some(found);
                }
            }
        }
    }
    None
}

/// Truncates to `max` characters (not bytes), adding "…".
pub fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Reads a field from the YAML frontmatter of a SKILL.md (not a full YAML parser:
/// single-line values and folded blocks ">-" / "|").
pub fn read_frontmatter_field(md_path: &Path, field: &str, max_len: usize) -> Option<String> {
    let text = String::from_utf8_lossy(&fs::read(md_path).ok()?).into_owned();
    let re = Regex::new(r"(?s)\A---\s*\n(.*?)\n---\s*\n").ok()?;
    let block = re.captures(&text)?.get(1)?.as_str().to_string();
    let lines: Vec<&str> = block.lines().collect();
    let prefix = format!("{field}:");

    for (i, line) in lines.iter().enumerate() {
        let Some(rest) = line.strip_prefix(&prefix) else {
            continue;
        };
        let value = rest.trim();
        let value = if matches!(value, ">" | ">-" | "|" | "|-") {
            let mut parts = Vec::new();
            for cont in &lines[i + 1..] {
                if cont.trim().is_empty() {
                    continue;
                }
                if !(cont.starts_with(' ') || cont.starts_with('\t')) {
                    break;
                }
                parts.push(cont.trim().to_string());
            }
            parts.join(" ")
        } else {
            value.trim_matches('"').to_string()
        };
        if value.is_empty() {
            return None;
        }
        return Some(truncate_chars(&value, max_len));
    }
    None
}

/// A SQLite value as bytes, whether text or blob (Cursor stores either depending on the version).
pub fn sql_bytes(value: rusqlite::types::Value) -> Option<Vec<u8>> {
    match value {
        rusqlite::types::Value::Text(s) => Some(s.into_bytes()),
        rusqlite::types::Value::Blob(b) => Some(b),
        _ => None,
    }
}
