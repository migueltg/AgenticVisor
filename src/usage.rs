//! Token usage of Claude Code and Claude Desktop sessions.
//!
//! What is read and why:
//! - Each assistant response carries `message.usage` (input, output,
//!   cache reads and cache writes) and `message.model`.
//! - The same response appears several times in the file (once per streaming
//!   chunk) with the same numbers. It is deduplicated by `message.id`; if
//!   there is no id, by the line's `uuid`. Summing without deduplicating over-counts.
//! - `cost-state` records hold the session's total cost, which is used
//!   as a cross-check.
//!
//! Cursor is not included: its chats do not store reliable token counters
//! (the fields exist but are zero or empty).

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::Serialize;
use serde_json::Value;

#[derive(Serialize, Clone, Copy, Debug, Default, PartialEq)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Tokens {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }

    pub fn add(&mut self, other: &Tokens) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
    }
}

/// Usage of a session, broken down by model.
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct SessionUsage {
    pub by_model: BTreeMap<String, Tokens>,
    /// Number of distinct assistant responses with recorded usage.
    pub responses: u64,
    /// Total cost Claude Code recorded for the session, if it recorded one.
    pub cost_usd: Option<f64>,
}

impl SessionUsage {
    pub fn totals(&self) -> Tokens {
        let mut totals = Tokens::default();
        for tokens in self.by_model.values() {
            totals.add(tokens);
        }
        totals
    }
}

fn tokens_from(usage: &Value) -> Tokens {
    let field = |name: &str| usage.get(name).and_then(Value::as_u64).unwrap_or(0);
    Tokens {
        input: field("input_tokens"),
        output: field("output_tokens"),
        cache_read: field("cache_read_input_tokens"),
        cache_write: field("cache_creation_input_tokens"),
    }
}

/// Reads a full transcript and returns its usage. None if there is nothing to count.
pub fn parse_session(path: &Path) -> Option<SessionUsage> {
    let file = File::open(path).ok()?;
    // Key: response id. Value: (model, tokens). The last
    // occurrence is kept, since it carries the final streaming count.
    let mut responses: HashMap<String, (String, Tokens)> = HashMap::new();
    let mut cost_usd = None;

    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let has_usage = line.contains("\"usage\"");
        let has_cost = line.contains("\"cost-state\"");
        if !has_usage && !has_cost {
            continue;
        }
        let Ok(d) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match d.get("type").and_then(Value::as_str) {
            Some("cost-state") => {
                if let Some(cost) = d.get("totalCostUSD").and_then(Value::as_f64) {
                    cost_usd = Some(cost);
                }
            }
            Some("assistant") => {
                let Some(message) = d.get("message") else {
                    continue;
                };
                let Some(usage) = message.get("usage").filter(|u| u.is_object()) else {
                    continue;
                };
                let key = message
                    .get("id")
                    .and_then(Value::as_str)
                    .or_else(|| d.get("uuid").and_then(Value::as_str))
                    .map(str::to_string);
                let Some(key) = key else {
                    continue;
                };
                let model = message
                    .get("model")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                responses.insert(key, (model, tokens_from(usage)));
            }
            _ => {}
        }
    }

    if responses.is_empty() && cost_usd.is_none() {
        return None;
    }
    let mut usage = SessionUsage { cost_usd, ..Default::default() };
    for (model, tokens) in responses.into_values() {
        usage.by_model.entry(model).or_default().add(&tokens);
        usage.responses += 1;
    }
    Some(usage)
}

type Cache = HashMap<PathBuf, ((u64, i128), Option<SessionUsage>)>;

fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Like `parse_session`, but reuses the result while the file does not
/// change (size and mtime). Transcripts can weigh hundreds of MB.
pub fn session_usage(path: &Path) -> Option<SessionUsage> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime_ns = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos() as i128;
    let signature = (meta.len(), mtime_ns);

    if let Ok(cache) = cache().lock() {
        if let Some((cached_sig, value)) = cache.get(path) {
            if *cached_sig == signature {
                return value.clone();
            }
        }
    }
    let value = parse_session(path);
    if let Ok(mut cache) = cache().lock() {
        cache.insert(path.to_path_buf(), (signature, value.clone()));
    }
    value
}

/// A Claude session with its usage, ready for the tokens view.
#[derive(Serialize, Clone, Debug)]
pub struct SessionRecord {
    pub tool: String,
    pub id: String,
    pub path: String,
    pub title: String,
    pub usage: SessionUsage,
}

/// Walks `dir` looking for .jsonl files. Does not follow symlinks (avoids loops). No limit
/// on depth: in Claude Desktop, subagents sit quite deep in the tree.
fn collect_jsonl(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    let mut items: Vec<_> = read.flatten().collect();
    items.sort_by_key(|e| e.path());
    for item in items {
        let Ok(file_type) = item.file_type() else {
            continue;
        };
        let path = item.path();
        if file_type.is_dir() {
            collect_jsonl(&path, out);
        } else if file_type.is_file() && path.extension().is_some_and(|e| e == "jsonl") {
            out.push(path);
        }
    }
}

/// Walks the Claude Code and Claude Desktop transcripts and returns the usage of
/// each session that has data. Independent of the scanner, which works at folder level.
pub fn collect(home: &Path) -> Vec<SessionRecord> {
    use crate::titles::{claude_code_ai_title, claude_code_fallback_title};

    let mut files = Vec::new();
    collect_jsonl(&home.join(".claude").join("projects"), &mut files);
    collect_jsonl(&home.join(".claude").join("transcripts"), &mut files);
    let desktop = home.join("Library").join("Application Support").join("Claude").join("local-agent-mode-sessions");
    collect_jsonl(&desktop, &mut files);

    let mut records = Vec::new();
    for file in files {
        let Some(usage) = session_usage(&file) else {
            continue;
        };
        let tool = if file.starts_with(&desktop) { "claude-desktop" } else { "claude-code" };
        let title = claude_code_ai_title(&file)
            .or_else(|| claude_code_fallback_title(&file))
            .unwrap_or_else(|| file.file_stem().unwrap_or_default().to_string_lossy().into_owned());
        records.push(SessionRecord {
            tool: tool.to_string(),
            id: file.to_string_lossy().into_owned(),
            path: file.to_string_lossy().into_owned(),
            title,
            usage,
        });
    }
    records
}

/// Aggregate for /api/usage: totals, per model, per tool and the most expensive sessions.
pub fn summary(records: &[SessionRecord]) -> serde_json::Value {
    let mut by_model: BTreeMap<String, Tokens> = BTreeMap::new();
    let mut by_tool: BTreeMap<String, Tokens> = BTreeMap::new();
    let mut totals = Tokens::default();
    let mut cost = 0.0f64;
    let mut cost_sessions = 0u64;
    for r in records {
        for (model, tokens) in &r.usage.by_model {
            by_model.entry(model.clone()).or_default().add(tokens);
            by_tool.entry(r.tool.clone()).or_default().add(tokens);
            totals.add(tokens);
        }
        if let Some(c) = r.usage.cost_usd {
            cost += c;
            cost_sessions += 1;
        }
    }
    let mut top: Vec<&SessionRecord> = records.iter().collect();
    top.sort_by(|a, b| b.usage.totals().total().cmp(&a.usage.totals().total()));
    let top: Vec<serde_json::Value> = top
        .into_iter()
        .take(15)
        .map(|r| {
            serde_json::json!({
                "tool": r.tool, "id": r.id, "path": r.path, "title": r.title,
                "total": r.usage.totals().total(), "cost_usd": r.usage.cost_usd,
            })
        })
        .collect();
    serde_json::json!({
        "sessions": records.len(),
        "totals": with_total(totals),
        "cost_usd": if cost_sessions > 0 { Some(cost) } else { None },
        "cost_sessions": cost_sessions,
        "by_model": by_model.iter().map(|(k, v)| (k.clone(), with_total(*v))).collect::<BTreeMap<_, _>>(),
        "by_tool": by_tool.iter().map(|(k, v)| (k.clone(), with_total(*v))).collect::<BTreeMap<_, _>>(),
        "top_sessions": top,
    })
}

fn with_total(t: Tokens) -> serde_json::Value {
    serde_json::json!({
        "input": t.input, "output": t.output, "cache_read": t.cache_read,
        "cache_write": t.cache_write, "total": t.total(),
    })
}
