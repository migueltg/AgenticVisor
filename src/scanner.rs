//! Scan entry point: runs each adapter, then the generic discovery
//! over whatever they do not cover.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::adapters::{claude_code, claude_desktop, codex, cursor_cli, cursor_desktop, generic, hermes};
use crate::entry::Entry;
use crate::fsutil::created_secs;

pub struct Scan {
    pub entries: Vec<Entry>,
}

pub fn run_scan(home: &Path) -> Scan {
    // Each adapter reads its own folders, so they run in parallel. The results are
    // joined in this fixed order, which keeps the output deterministic.
    let adapters: [fn(&Path) -> Vec<Entry>; 6] = [
        claude_code::scan,
        claude_desktop::scan,
        cursor_cli::scan,
        cursor_desktop::scan,
        codex::scan,
        hermes::scan,
    ];
    let mut entries = Vec::new();
    std::thread::scope(|scope| {
        let handles: Vec<_> = adapters.iter().map(|&adapter| scope.spawn(move || adapter(home))).collect();
        for handle in handles {
            entries.extend(handle.join().unwrap_or_default());
        }
    });

    let app_support = home.join("Library").join("Application Support");
    let claimed: HashSet<String> = [
        home.join(".claude"),
        home.join(".claude.json"),
        home.join(".claude.json.backup"),
        app_support.join("Claude"),
        home.join(".cursor"),
        app_support.join("Cursor"),
        home.join(".codex"),
        home.join(".hermes"),
    ]
    .iter()
    .map(|p| p.to_string_lossy().into_owned())
    .collect();
    entries.extend(generic::scan(home, &claimed));

    // Creation time comes from the file system, so it is read here once for every entry.
    for entry in &mut entries {
        if entry.created == 0.0 {
            entry.created = created_secs(Path::new(&entry.path));
        }
    }

    // Sort by size, largest first. Rust's sort is stable.
    entries.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
    Scan { entries }
}

fn now_secs() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// Response of /api/scan: the same keys plus token usage.
pub fn scan_json(home: &Path) -> Value {
    let now = now_secs();
    let scan = run_scan(home);
    let total_size: u64 = scan.entries.iter().map(|e| e.size_bytes).sum();

    let mut by_tool: BTreeMap<String, u64> = BTreeMap::new();
    let mut by_category: BTreeMap<String, u64> = BTreeMap::new();
    for e in &scan.entries {
        *by_tool.entry(e.tool.clone()).or_default() += e.size_bytes;
        *by_category.entry(e.category.clone()).or_default() += e.size_bytes;
    }

    let entries: Vec<Value> = scan.entries.iter().map(|e| e.to_json(now)).collect();
    json!({
        "generated_at": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        "entries": entries,
        "summary": {
            "total_size_bytes": total_size,
            "total_entries": scan.entries.len(),
            "by_tool": by_tool,
            "by_category": by_category,
        }
    })
}
