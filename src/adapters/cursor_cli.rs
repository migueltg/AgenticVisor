//! Adapter for ~/.cursor (Cursor CLI / agent).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::entry::Entry;
use crate::fsutil::{classify_by_name, dir_stats, mtime_secs, read_frontmatter_field, resolve_dashed_path};
use crate::titles::{cursor_chat_title, cursor_transcript_title};

const TRANSCRIPTS_DIR: &str = "agent-transcripts";

const KNOWN_SUBDIRS: &[(&str, &str, bool, &str)] = &[
    ("ai-tracking", "cache", false, "AI usage telemetry/analytics"),
    ("snapshots", "cache", false, "Sandbox snapshots"),
    ("agents", "skills", false, "Agentes configurados"),
    ("sandbox-policies", "config", false, "Sandbox policies"),
];

const HANDLED_SEPARATELY: &[&str] = &["projects", "chats", "extensions", "plugins", "skills-cursor", "plans"];

const CONFIG_FILES: &[(&str, &str, bool)] = &[
    ("hooks.json", "Hooks configurados", false),
    ("cli-config.json", "CLI configuration", false),
    ("argv.json", "Startup flags", false),
    ("agent-cli-state.json", "Agent state", true),
    ("statsig-cache.json", "Feature flag cache", false),
    ("ide_state.json", "IDE state", false),
];

fn sorted_children(dir: &Path) -> Vec<PathBuf> {
    let mut items: Vec<PathBuf> = fs::read_dir(dir).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    items.sort();
    items
}

fn file_name(path: &Path) -> String {
    path.file_name().unwrap_or_default().to_string_lossy().into_owned()
}

/// Category, title and note of a Cursor project, derived from its folder name.
/// macOS temporary workspaces are marked as orphans; names made only of
/// digits are not paths and are not treated as orphans.
///
/// The fourth value is the project path, only when the folder name could be turned back
/// into a path that exists: a path known from somewhere else, or one found on disk.
fn classify_project_dir(dirname: &str, known: &HashMap<String, String>) -> (&'static str, String, String, Option<String>) {
    // A path known from somewhere else wins over any guess made from the name.
    if let Some(path) = crate::cursor_sessions::project_for_slug(known, dirname) {
        return ("sessions", path.clone(), String::new(), Some(path));
    }
    if dirname.starts_with("var-folders-") {
        return (
            "orphaned",
            dirname.to_string(),
            "macOS temporary workspace (/var/folders/...), almost certainly obsolete".to_string(),
            None,
        );
    }
    if !dirname.is_empty() && dirname.chars().all(|c| c.is_ascii_digit()) {
        return ("other", dirname.to_string(), "Numeric folder: cannot be matched to any project".to_string(), None);
    }
    if let Some(path) = resolve_dashed_path(dirname, "/") {
        return ("sessions", path.clone(), String::new(), Some(path));
    }
    (
        "orphaned",
        format!("/{}", dirname.replace('-', "/")),
        "No disk path matches this folder name: probably a deleted or moved project".to_string(),
        None,
    )
}

fn scan_agent_transcripts(proj: &Path, project_title: &str, project: Option<&str>, out: &mut Vec<Entry>) {
    let dir = proj.join(TRANSCRIPTS_DIR);
    if !dir.is_dir() {
        return;
    }
    for conv in sorted_children(&dir).into_iter().filter(|p| p.is_dir()) {
        let (size, count, mtime) = dir_stats(&conv, &[]);
        let jsonl = conv.join(format!("{}.jsonl", file_name(&conv)));
        let title = if jsonl.is_file() {
            cursor_transcript_title(&jsonl)
        } else {
            None
        }
        .unwrap_or_else(|| "(no user messages)".to_string());
        let mut entry = Entry::new(&conv.to_string_lossy(), "cursor-cli", "sessions", &conv.to_string_lossy())
            .with_stats(size, count, mtime)
            .title(title)
            .note(format!("Agent conversation · Project: {project_title}"));
        if let Some(project) = project {
            entry = entry.project(project);
        }
        out.push(entry);
    }
}

fn scan_chats(root: &Path, out: &mut Vec<Entry>) {
    let chats = root.join("chats");
    if !chats.is_dir() {
        return;
    }
    for hash_dir in sorted_children(&chats).into_iter().filter(|p| p.is_dir()) {
        for chat_dir in sorted_children(&hash_dir).into_iter().filter(|p| p.is_dir()) {
            let db = chat_dir.join("store.db");
            let title = if db.is_file() { cursor_chat_title(&db) } else { None }
                .unwrap_or_else(|| "(untitled)".to_string());
            let (size, count, mtime) = dir_stats(&chat_dir, &[]);
            let mut entry = Entry::new(&chat_dir.to_string_lossy(), "cursor-cli", "sessions", &chat_dir.to_string_lossy())
                .with_stats(size, count, mtime)
                .title(title)
                .note("Cursor chat");
            // The chat's metadata records the folder it was started in.
            let cwd = fs::read_to_string(chat_dir.join("meta.json"))
                .ok()
                .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                .and_then(|meta| meta.get("cwd").and_then(|c| c.as_str()).map(|c| c.trim_end_matches('/').to_string()))
                .filter(|c| !c.is_empty());
            if let Some(cwd) = cwd {
                entry.note = format!("Cursor chat · Project: {cwd}");
                entry = entry.project(cwd);
            }
            out.push(entry);
        }
    }
}

fn scan_extensions(root: &Path, out: &mut Vec<Entry>) {
    let dir = root.join("extensions");
    if !dir.is_dir() {
        return;
    }
    for item in sorted_children(&dir).into_iter().filter(|p| p.is_dir()) {
        let (size, count, mtime) = dir_stats(&item, &[]);
        out.push(
            Entry::new(&item.to_string_lossy(), "cursor-cli", "plugins", &item.to_string_lossy())
                .with_stats(size, count, mtime)
                .title(file_name(&item))
                .note("Installed extension"),
        );
    }
}

fn scan_skills(root: &Path, out: &mut Vec<Entry>) {
    let dir = root.join("skills-cursor");
    if !dir.is_dir() {
        return;
    }
    for item in sorted_children(&dir).into_iter().filter(|p| p.is_dir()) {
        let (size, count, mtime) = dir_stats(&item, &[]);
        let description = read_frontmatter_field(&item.join("SKILL.md"), "description", 200).unwrap_or_default();
        out.push(
            Entry::new(&item.to_string_lossy(), "cursor-cli", "skills", &item.to_string_lossy())
                .with_stats(size, count, mtime)
                .title(file_name(&item))
                .note(description),
        );
    }
}

fn scan_plugins(root: &Path, out: &mut Vec<Entry>) {
    let plugins_root = root.join("plugins");
    if !plugins_root.is_dir() {
        return;
    }
    for (subdir_name, category, note) in [
        ("cache", "cache", "Plugin cacheado, redescargable"),
        ("local", "plugins", "Plugin instalado localmente"),
    ] {
        let subdir = plugins_root.join(subdir_name);
        if !subdir.is_dir() {
            continue;
        }
        for item in sorted_children(&subdir).into_iter().filter(|p| p.is_dir()) {
            let (size, count, mtime) = dir_stats(&item, &[]);
            out.push(
                Entry::new(&item.to_string_lossy(), "cursor-cli", category, &item.to_string_lossy())
                    .with_stats(size, count, mtime)
                    .title(file_name(&item))
                    .note(note),
            );
        }
    }
}

fn scan_mcp_servers(root: &Path, out: &mut Vec<Entry>) {
    let file = root.join("mcp.json");
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
                "cursor-cli",
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
    let root = home.join(".cursor");
    if !root.is_dir() {
        return out;
    }

    let projects_dir = root.join("projects");
    if projects_dir.is_dir() {
        let known = crate::cursor_sessions::known_paths(home);
        for proj in sorted_children(&projects_dir).into_iter().filter(|p| p.is_dir()) {
            let name = file_name(&proj);
            let (category, title, note, project) = classify_project_dir(&name, &known);
            let (size, count, mtime) = dir_stats(&proj, &[TRANSCRIPTS_DIR]);
            let mut entry = Entry::new(&proj.to_string_lossy(), "cursor-cli", category, &proj.to_string_lossy())
                .with_stats(size, count, mtime)
                .title(title.clone())
                .note(note);
            // What hangs from the project folder (canvases, MCP state, terminals) belongs to the project.
            if let Some(project) = &project {
                entry = entry.project(project.clone());
            }
            out.push(entry);
            scan_agent_transcripts(&proj, &title, project.as_deref(), &mut out);
        }
    }

    scan_chats(&root, &mut out);
    scan_extensions(&root, &mut out);
    scan_skills(&root, &mut out);
    scan_plugins(&root, &mut out);
    scan_mcp_servers(&root, &mut out);
    out.extend(crate::plans::scan_cursor(&root));

    for child in sorted_children(&root).into_iter().filter(|p| p.is_dir()) {
        let name = file_name(&child);
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
            Entry::new(&child.to_string_lossy(), "cursor-cli", &category, &child.to_string_lossy())
                .with_stats(size, count, mtime)
                .sensitive(sensitive)
                .note(note),
        );
    }

    for (fname, note, sensitive) in CONFIG_FILES {
        let file = root.join(fname);
        if file.is_file() {
            if let Ok(meta) = fs::metadata(&file) {
                out.push(
                    Entry::new(&file.to_string_lossy(), "cursor-cli", "config", &file.to_string_lossy())
                        .with_stats(meta.len(), 1, mtime_secs(&meta))
                        .sensitive(*sensitive)
                        .note(*note),
                );
            }
        }
    }
    out
}
