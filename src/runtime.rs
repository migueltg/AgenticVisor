//! Which agent programs are running right now. Used to show a status per tool and
//! to refuse changes to a database that the app keeps open.

use std::process::Command;

use serde_json::{json, Value};

/// Executable names that belong to each tool. Matched exactly, after dropping ".exe".
const TOOL_PROCESSES: &[(&str, &[&str])] = &[
    ("claude-code", &["claude"]),
    ("claude-desktop", &["Claude"]),
    ("cursor-cli", &["cursor-agent"]),
    ("cursor-desktop", &["Cursor"]),
    ("codex", &["codex"]),
    ("hermes", &["hermes"]),
];

/// Names of the running processes. Uses `tasklist` on Windows and `ps` elsewhere.
/// Returns an empty list if the command cannot be run.
pub fn process_names() -> Vec<String> {
    let output = if cfg!(windows) {
        Command::new("tasklist").args(["/FO", "CSV", "/NH"]).output()
    } else {
        Command::new("ps").args(["-axo", "comm="]).output()
    };
    match output {
        Ok(out) => parse_process_names(&String::from_utf8_lossy(&out.stdout)),
        Err(_) => Vec::new(),
    }
}

/// Bare executable names from the output of `ps -axo comm=` (full paths) or
/// `tasklist /FO CSV /NH` (quoted CSV rows).
pub fn parse_process_names(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            let name = match line.strip_prefix('"') {
                Some(csv) => csv.split('"').next().unwrap_or_default().to_string(),
                None => line.rsplit('/').next().unwrap_or(line).to_string(),
            };
            name.strip_suffix(".exe").map(str::to_string).unwrap_or(name)
        })
        .collect()
}

/// Whether the program of `tool` appears in `names`.
pub fn is_running(names: &[String], tool: &str) -> bool {
    process_names_of(tool).iter().any(|p| names.iter().any(|n| n == p))
}

fn process_names_of(tool: &str) -> &'static [&'static str] {
    TOOL_PROCESSES.iter().find(|(t, _)| *t == tool).map(|(_, p)| *p).unwrap_or(&[])
}

/// Status per tool for /api/runtime: whether it runs and how many of its processes do.
pub fn status(names: &[String]) -> Value {
    let mut tools = serde_json::Map::new();
    for (tool, processes) in TOOL_PROCESSES {
        let count = names.iter().filter(|n| processes.contains(&n.as_str())).count();
        tools.insert(tool.to_string(), json!({"running": count > 0, "processes": count}));
    }
    json!({"tools": tools})
}
