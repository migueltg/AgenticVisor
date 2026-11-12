//! Adapter for the Claude desktop app (~/Library/Application Support/Claude).
//! Alternative profiles that the
//! app creates have no adapter of their own: generic discovery picks them up.

use std::fs;
use std::path::Path;

use crate::entry::Entry;
use crate::fsutil::{classify_by_name, dir_stats, mtime_secs};

/// Known files and folders: (name, category, sensitive, note).
const KNOWN_ENTRIES: &[(&str, &str, bool, &str)] = &[
    ("claude-code-sessions", "sessions", false, "Claude Code sessions started from the app"),
    ("claude-code-vm", "cache", false, "Local virtual machine to run Claude Code"),
    ("claude-code", "cache", false, "Claude Code binaries/state embedded in the app"),
    ("local-agent-mode-sessions", "sessions", false, "\"agent mode\" sessions"),
    ("local-agent-mode-sessions_", "sessions", false, "\"agent mode\" sessions (variant)"),
    ("vm_bundles", "cache", false, "Local VM bundles"),
    ("git-worktrees.json", "sessions", false, "Git worktrees used by agent mode"),
    ("git-shadow", "sessions", false, "Shadow copies of repos used by agent mode"),
    ("buddy-tokens.json", "config", true, "Authentication tokens"),
    ("claude_desktop_config.json", "config", true, "MCP server configuration"),
    ("Cookies", "config", true, "Session cookies of the embedded browser"),
    ("Cookies-journal", "config", true, "Journal of the session cookies"),
    ("config.json", "config", true, "General app configuration"),
    ("bridge-state.json", "config", false, "State of the app's native bridge"),
    ("sentry", "cache", false, "Error reports sent to Sentry"),
    ("DIPS", "other", false, "Chromium internal state (tracking prevention)"),
    ("DIPS-wal", "other", false, "Journal of the above"),
    ("TransportSecurity", "other", false, "Chromium HSTS list (which domains to force over HTTPS)"),
    ("Trust Tokens", "config", true, "Chromium Trust Tokens (anti-fraud mechanism, not your credentials)"),
    ("Trust Tokens-journal", "config", true, "Journal of the above"),
    ("SharedStorage", "cache", false, "Chromium cross-origin shared storage"),
    ("SharedStorage-wal", "cache", false, "Journal of the above"),
    ("Network Persistent State", "other", false, "Chromium network state (DNS cache, etc.)"),
    ("ant-did", "other", false, "App device identifier (telemetry)"),
    ("ant-device-registry.json", "other", false, "Device registration with Anthropic's servers"),
    ("window-state.json", "other", false, "App window size/position"),
    ("cowork-enabled-cli-ops.json", "other", false, "Internal flags for collaboration features"),
    ("extensions-blocklist.json", "other", false, "List of extensions blocked by the app"),
    ("InterestGroups", "other", false, "Chromium Privacy Sandbox data, not used by Claude"),
    ("pending-uploads", "other", false, "Queue of pending file uploads"),
    ("document-baselines", "other", false, "Internal state of the document editor"),
    ("plan-usage-history.json", "sessions", false, "Your plan's usage history (messages/limits over time)"),
    ("vm-support-probe.json", "other", false, "Result of checking whether your machine supports the local VM"),
    ("ca-bundle.pem", "other", false, "Root certificates the app uses for HTTPS"),
    (".DS_Store", "other", false, "macOS Finder metadata, not from the app"),
];

pub fn scan(home: &Path) -> Vec<Entry> {
    let mut out = Vec::new();
    let root = home.join("Library").join("Application Support").join("Claude");
    let Ok(read) = fs::read_dir(&root) else {
        return out;
    };
    let mut children: Vec<_> = read.flatten().map(|e| e.path()).collect();
    children.sort();

    for child in children {
        let name = child.file_name().unwrap_or_default().to_string_lossy().into_owned();
        // Agent mode sessions are listed one by one, not as a single folder.
        if name == "local-agent-mode-sessions" && child.is_dir() {
            out.extend(crate::desktop_sessions::agent_mode_entries(&child));
            continue;
        }
        let (category, sensitive, note) = match KNOWN_ENTRIES.iter().find(|(n, ..)| *n == name) {
            Some((_, cat, sens, note)) => (cat.to_string(), *sens, note.to_string()),
            None => {
                let (cat, sens) = classify_by_name(&name);
                (cat.to_string(), sens, String::new())
            }
        };
        let (size, count, mtime) = if child.is_dir() {
            dir_stats(&child, &[])
        } else {
            match fs::metadata(&child) {
                Ok(meta) => (meta.len(), 1, mtime_secs(&meta)),
                Err(_) => continue,
            }
        };
        out.push(
            Entry::new(&child.to_string_lossy(), "claude-desktop", &category, &child.to_string_lossy())
                .with_stats(size, count, mtime)
                .sensitive(sensitive)
                .note(note),
        );
    }
    out
}
