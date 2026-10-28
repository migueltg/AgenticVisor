//! Work plans written by the agents, one entry per plan file.
//!
//! Claude Code keeps plans as Markdown in ~/.claude/plans, titled by their first
//! heading. Cursor keeps them in ~/.cursor/plans with a YAML header holding the
//! name, an overview and the list of tasks. Plans do not record the project they
//! belong to, so they carry no project.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;

use crate::adapters::sorted_children;
use crate::entry::Entry;
use crate::fsutil::{created_secs, mtime_secs, read_frontmatter_field};
use crate::titles::clean_title;

/// Longest overview kept in a plan's note, in characters.
const OVERVIEW_CHARS: usize = 200;

/// One entry per plan in Cursor's plans folder under `cursor_root` (~/.cursor).
pub fn scan_cursor(cursor_root: &Path) -> Vec<Entry> {
    plan_entries(&cursor_root.join("plans"), "cursor-cli", cursor_plan)
}

/// One entry per plan in Claude Code's plans folder under `claude_root` (~/.claude).
pub fn scan_claude(claude_root: &Path) -> Vec<Entry> {
    plan_entries(&claude_root.join("plans"), "claude-code", claude_plan)
}

fn plan_entries(dir: &Path, tool: &str, describe: fn(&Path) -> (String, String)) -> Vec<Entry> {
    let mut out = Vec::new();
    for file in sorted_children(dir) {
        let is_markdown = file.extension().is_some_and(|e| e == "md");
        if !is_markdown || !file.is_file() {
            continue;
        }
        let Ok(meta) = fs::metadata(&file) else {
            continue;
        };
        let (title, note) = describe(&file);
        let id = file.to_string_lossy().into_owned();
        let mut entry = Entry::new(&id, tool, "sessions", &id)
            .with_stats(meta.len(), 1, mtime_secs(&meta))
            .title(title)
            .note(note);
        entry.created = created_secs(&file);
        out.push(entry);
    }
    out
}

fn file_stem(path: &Path) -> String {
    path.file_stem().unwrap_or_default().to_string_lossy().into_owned()
}

/// Title and note of a Claude Code plan: the first heading, and a fixed note.
fn claude_plan(path: &Path) -> (String, String) {
    let heading = fs::read_to_string(path).ok().and_then(|text| {
        text.lines()
            .find_map(|line| line.strip_prefix("# ").map(|t| t.trim().to_string()))
    });
    let title = heading.map(|h| clean_title(&h)).unwrap_or_else(|| file_stem(path));
    (title, "Work plan (Claude Code)".to_string())
}

/// Title and note of a Cursor plan: the header's name, overview and task progress.
fn cursor_plan(path: &Path) -> (String, String) {
    let title = read_frontmatter_field(path, "name", 90).unwrap_or_else(|| file_stem(path));
    let mut parts = Vec::new();
    if let Some(overview) = read_frontmatter_field(path, "overview", OVERVIEW_CHARS) {
        parts.push(overview);
    }
    if let Some((done, total)) = task_progress(path) {
        parts.push(format!("{done} of {total} tasks done"));
    }
    (title, parts.join(" · "))
}

/// Tasks in the YAML header: (completed, total). None when the header has no tasks.
fn task_progress(path: &Path) -> Option<(usize, usize)> {
    let file = fs::File::open(path).ok()?;
    let mut lines = BufReader::new(file).lines().map_while(Result::ok);
    if lines.next()?.trim() != "---" {
        return None;
    }
    let (mut done, mut total) = (0usize, 0usize);
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        let trimmed = line.trim();
        if trimmed.starts_with("- id:") {
            total += 1;
        } else if trimmed == "status: completed" {
            done += 1;
        }
    }
    (total > 0).then_some((done, total))
}
