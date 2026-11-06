//! Adapter for ~/.claude (Claude Code CLI) and its registry ~/.claude.json.
//! Equivalente a adapters/claude_code.py.

use std::fs;
use std::path::{Path, PathBuf};

use crate::entry::Entry;
use crate::fsutil::{classify_by_name, dir_stats, mtime_secs, read_frontmatter_field, resolve_dashed_path};
use crate::titles::{
    claude_code_ai_title, claude_code_fallback_title, claude_code_history_index, claude_transcript_title,
};
use crate::usage::session_usage;

const SESSION_SIDECAR_SKIP: &[&str] = &[];

/// Known ~/.claude subfolders: (name, category, sensitive, note).
const KNOWN_SUBDIRS: &[(&str, &str, bool, &str)] = &[
    ("tasks", "sessions", false, "Background tasks"),
    ("jobs", "sessions", false, "Jobs programados"),
    ("file-history", "cache", false, "Copies of edited files, to undo changes"),
    ("shell-snapshots", "cache", false, "Shell snapshots taken when each session starts"),
    ("paste-cache", "cache", false, "Content pasted into the chat, cached"),
    ("session-env", "cache", false, "Ephemeral environment per session"),
    ("cache", "cache", false, "Claude Code internal cache"),
    ("backups", "cache", false, "Configuration backups"),
    ("daemon", "cache", false, "Local daemon state"),
];

const HANDLED_SEPARATELY: &[&str] = &["projects", "sessions", "transcripts", "skills", "plugins", "plans"];

fn cwd_from_transcripts(project_folder: &Path) -> Option<String> {
    let mut files: Vec<PathBuf> = fs::read_dir(project_folder)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    files.sort();
    for file in files {
        let Ok(text) = fs::read_to_string(&file) else {
            continue;
        };
        for line in text.lines().take(30) {
            let Ok(d) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if let Some(cwd) = d.get("cwd").and_then(|v| v.as_str()) {
                if !cwd.is_empty() {
                    return Some(cwd.to_string());
                }
            }
        }
    }
    None
}

/// (path, exists_on_disk) of the project. First the "cwd" from the transcripts;
/// if none is readable, it is rebuilt from the folder name.
fn project_path(project_folder: &Path) -> (String, bool) {
    if let Some(cwd) = cwd_from_transcripts(project_folder) {
        let exists = Path::new(&cwd).exists();
        return (cwd, exists);
    }
    let name = project_folder.file_name().unwrap_or_default().to_string_lossy().into_owned();
    if let Some(resolved) = resolve_dashed_path(&name, "/") {
        return (resolved, true);
    }
    (format!("/{}", name.trim_matches('-').replace('-', "/")), false)
}

fn sorted_children(dir: &Path) -> Vec<PathBuf> {
    let mut items: Vec<PathBuf> = fs::read_dir(dir).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    items.sort();
    items
}

fn is_jsonl(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "jsonl")
}

fn scan_projects(root: &Path, home: &Path, out: &mut Vec<Entry>) {
    let projects_dir = root.join("projects");
    if !projects_dir.is_dir() {
        return;
    }
    let history = claude_code_history_index(home);

    for project_folder in sorted_children(&projects_dir).into_iter().filter(|p| p.is_dir()) {
        let (project, exists) = project_path(&project_folder);
        let label = if exists { project.clone() } else { format!("{project} (no longer exists on disk)") };

        let mut handled: Vec<PathBuf> = Vec::new();
        let jsonls: Vec<PathBuf> = sorted_children(&project_folder).into_iter().filter(|p| is_jsonl(p) && p.is_file()).collect();
        for jsonl in jsonls {
            let Some(session_id) = jsonl.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
                continue;
            };
            handled.push(jsonl.clone());
            let Ok(meta) = fs::metadata(&jsonl) else {
                continue;
            };
            let (mut size, mut count, mut mtime) = (meta.len(), 1u64, mtime_secs(&meta));

            let sidecar = project_folder.join(&session_id);
            if sidecar.is_dir() {
                handled.push(sidecar.clone());
                let (s, c, m) = dir_stats(&sidecar, SESSION_SIDECAR_SKIP);
                size += s;
                count += c;
                mtime = mtime.max(m);
            }

            let title = claude_code_ai_title(&jsonl)
                .or_else(|| history.get(&session_id).cloned())
                .or_else(|| claude_code_fallback_title(&jsonl))
                .unwrap_or_else(|| "(no user messages)".to_string());

            let mut entry = Entry::new(&jsonl.to_string_lossy(), "claude-code", "sessions", &jsonl.to_string_lossy())
                .with_stats(size, count, mtime)
                .title(title)
                .project(project.clone())
                .note(format!("Project: {label}"));
            entry.usage = session_usage(&jsonl);
            out.push(entry);
        }

        for item in sorted_children(&project_folder) {
            if handled.contains(&item) {
                continue;
            }
            let name = item.file_name().unwrap_or_default().to_string_lossy().into_owned();
            let (category, extra) = if name == "memory" {
                ("other", "Persistent project memory — neither cache nor a session")
            } else {
                (classify_by_name(&name).0, "")
            };
            let Ok(meta) = fs::metadata(&item) else {
                continue;
            };
            let (size, count, mtime) = if item.is_dir() {
                dir_stats(&item, &[])
            } else {
                (meta.len(), 1, mtime_secs(&meta))
            };
            let note = if extra.is_empty() {
                format!("Project: {label}")
            } else {
                format!("Project: {label} — {extra}")
            };
            out.push(
                Entry::new(&item.to_string_lossy(), "claude-code", category, &item.to_string_lossy())
                    .with_stats(size, count, mtime)
                    .note(note),
            );
        }
    }
}

fn scan_claude_sessions(root: &Path, out: &mut Vec<Entry>) {
    let sessions_dir = root.join("sessions");
    if !sessions_dir.is_dir() {
        return;
    }
    let mut handled: Vec<PathBuf> = Vec::new();
    let json_files: Vec<PathBuf> = sorted_children(&sessions_dir)
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();

    for jf in json_files {
        handled.push(jf.clone());
        let Ok(meta) = fs::metadata(&jf) else {
            continue;
        };
        let Ok(text) = fs::read_to_string(&jf) else {
            continue;
        };
        let Ok(data) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let (mut size, mut count, mut mtime) = (meta.len(), 1u64, mtime_secs(&meta));
        let stem = jf.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        for key in sorted_children(&sessions_dir) {
            let name = key.file_name().unwrap_or_default().to_string_lossy().into_owned();
            if name.starts_with(&format!("{stem}.")) && name.ends_with(".key") {
                handled.push(key.clone());
                if let Ok(kmeta) = fs::metadata(&key) {
                    size += kmeta.len();
                    count += 1;
                    mtime = mtime.max(mtime_secs(&kmeta));
                }
            }
        }
        let title = data
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| {
                let pid = data.get("pid").map(|p| p.to_string()).unwrap_or_else(|| stem.clone());
                format!("session pid {pid}")
            });
        let status = data.get("status").and_then(|v| v.as_str()).unwrap_or("?");
        let cwd = data.get("cwd").and_then(|v| v.as_str()).unwrap_or("");
        out.push(
            Entry::new(&jf.to_string_lossy(), "claude-code", "sessions", &jf.to_string_lossy())
                .with_stats(size, count, mtime)
                .sensitive(true)
                .title(title)
                .note(format!("Proceso {status} · {cwd}")),
        );
    }

    for leftover in sorted_children(&sessions_dir) {
        if handled.contains(&leftover) {
            continue;
        }
        let Ok(meta) = fs::metadata(&leftover) else {
            continue;
        };
        out.push(
            Entry::new(&leftover.to_string_lossy(), "claude-code", "sessions", &leftover.to_string_lossy())
                .with_stats(meta.len(), 1, mtime_secs(&meta))
                .sensitive(true)
                .note("Session file without its paired .json (orphan)"),
        );
    }
}

fn scan_transcripts(root: &Path, out: &mut Vec<Entry>) {
    let dir = root.join("transcripts");
    if !dir.is_dir() {
        return;
    }
    for jsonl in sorted_children(&dir).into_iter().filter(|p| is_jsonl(p) && p.is_file()) {
        let Ok(meta) = fs::metadata(&jsonl) else {
            continue;
        };
        let title = claude_transcript_title(&jsonl).unwrap_or_else(|| "(no user messages)".to_string());
        let mut entry = Entry::new(&jsonl.to_string_lossy(), "claude-code", "sessions", &jsonl.to_string_lossy())
            .with_stats(meta.len(), 1, mtime_secs(&meta))
            .title(title)
            .note("Transcript (formato interno alternativo)");
        entry.usage = session_usage(&jsonl);
        out.push(entry);
    }
}

fn scan_skills(root: &Path, out: &mut Vec<Entry>) {
    let skills_dir = root.join("skills");
    if !skills_dir.is_dir() {
        return;
    }
    for item in sorted_children(&skills_dir) {
        let is_link = fs::symlink_metadata(&item).map(|m| m.file_type().is_symlink()).unwrap_or(false);
        let real = if is_link { fs::canonicalize(&item).unwrap_or(item.clone()) } else { item.clone() };
        if !real.is_dir() {
            continue;
        }
        let (size, count, mtime) = dir_stats(&real, &[]);
        let description = read_frontmatter_field(&real.join("SKILL.md"), "description", 200).unwrap_or_default();
        let note = if is_link {
            let link_note = format!(
                "symlink to {} — moving this only unlinks it from Claude Code, it does not delete the shared content",
                real.display()
            );
            if description.is_empty() { link_note } else { format!("{description} — {link_note}") }
        } else {
            description
        };
        let name = item.file_name().unwrap_or_default().to_string_lossy().into_owned();
        out.push(
            Entry::new(&item.to_string_lossy(), "claude-code", "skills", &item.to_string_lossy())
                .with_stats(size, count, mtime)
                .title(name)
                .note(note),
        );
    }
}

fn scan_plugins(root: &Path, out: &mut Vec<Entry>) {
    let plugins_root = root.join("plugins");
    if !plugins_root.is_dir() {
        return;
    }
    let groups = [
        ("data", "plugins", "Local data of an installed plugin"),
        ("marketplaces", "plugins", "Plugin marketplace (source code)"),
        ("cache", "cache", "Re-downloadable marketplace cache"),
    ];
    let mut handled_dirs: Vec<PathBuf> = Vec::new();
    for (subdir_name, category, note) in groups {
        let subdir = plugins_root.join(subdir_name);
        handled_dirs.push(subdir.clone());
        if !subdir.is_dir() {
            continue;
        }
        for item in sorted_children(&subdir).into_iter().filter(|p| p.is_dir()) {
            let (size, count, mtime) = dir_stats(&item, &[]);
            let name = item.file_name().unwrap_or_default().to_string_lossy().into_owned();
            out.push(
                Entry::new(&item.to_string_lossy(), "claude-code", category, &item.to_string_lossy())
                    .with_stats(size, count, mtime)
                    .title(name)
                    .note(note),
            );
        }
    }

    // Loose indexes at the root of plugins/ are grouped into one informational entry.
    let (mut rest_size, mut rest_count, mut rest_mtime) = (0u64, 0u64, 0.0f64);
    for item in sorted_children(&plugins_root) {
        if handled_dirs.contains(&item) || item.is_dir() {
            continue;
        }
        let Ok(meta) = fs::metadata(&item) else {
            continue;
        };
        rest_size += meta.len();
        rest_count += 1;
        rest_mtime = rest_mtime.max(mtime_secs(&meta));
    }
    if rest_count > 0 {
        out.push(
            Entry::new(
                &format!("{}#config", plugins_root.display()),
                "claude-code",
                "config",
                &plugins_root.to_string_lossy(),
            )
            .with_stats(rest_size, rest_count, rest_mtime)
            .informational()
            .note("Plugin system indexes (what is installed and from where) — informational only"),
        );
    }
}

/// MCP servers from ~/.claude.json: name and transport only, never commands or credentials.
fn scan_mcp_servers(home: &Path, out: &mut Vec<Entry>) {
    let file = home.join(".claude.json");
    let Ok(meta) = fs::metadata(&file) else {
        return;
    };
    let Ok(data) = fs::read_to_string(&file).and_then(|t| serde_json::from_str::<serde_json::Value>(&t).map_err(Into::into)) else {
        return;
    };
    let Some(servers) = data.get("mcpServers").and_then(|v| v.as_object()) else {
        return;
    };
    for (name, cfg) in servers {
        if !cfg.is_object() {
            continue;
        }
        let transport = cfg
            .get("type")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| if cfg.get("url").is_some() { "http".into() } else { "stdio".into() });
        out.push(
            Entry::new(
                &format!("{}#mcpServers.{}", file.display(), name),
                "claude-code",
                "config",
                &file.to_string_lossy(),
            )
            .with_stats(0, 0, mtime_secs(&meta))
            .sensitive(true)
            .informational()
            .title(name.clone())
            .note(format!("MCP server ({transport}) — informational only")),
        );
    }
}

pub fn scan(home: &Path) -> Vec<Entry> {
    let mut out = Vec::new();
    let root = home.join(".claude");
    if root.is_dir() {
        scan_projects(&root, home, &mut out);
        scan_claude_sessions(&root, &mut out);
        scan_transcripts(&root, &mut out);
        scan_skills(&root, &mut out);
        scan_plugins(&root, &mut out);
        out.extend(crate::plans::scan_claude(&root));

        for child in sorted_children(&root).into_iter().filter(|p| p.is_dir()) {
            let name = child.file_name().unwrap_or_default().to_string_lossy().into_owned();
            if HANDLED_SEPARATELY.contains(&name.as_str()) {
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
                Entry::new(&child.to_string_lossy(), "claude-code", &category, &child.to_string_lossy())
                    .with_stats(size, count, mtime)
                    .sensitive(sensitive)
                    .note(note),
            );
        }
    }

    scan_mcp_servers(home, &mut out);

    for (fname, note) in [
        (".claude.json", "Registry of projects and configured MCP servers"),
        (".claude.json.backup", "Backup of the previous registry"),
    ] {
        let file = home.join(fname);
        if let Ok(meta) = fs::metadata(&file) {
            if file.is_file() {
                out.push(
                    Entry::new(&file.to_string_lossy(), "claude-code", "config", &file.to_string_lossy())
                        .with_stats(meta.len(), 1, mtime_secs(&meta))
                        .sensitive(true)
                        .note(note),
                );
            }
        }
    }
    out
}
