//! A scan entry: what each adapter produces and what the interface shows.
//! Fields shared by every adapter.

use serde::Serialize;
use serde_json::{json, Value};

use crate::fsutil::human_size;
use crate::usage::SessionUsage;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Entry {
    /// Stable identifier: usually the absolute path.
    pub id: String,
    /// "claude-code" | "claude-desktop" | "cursor-cli" | "cursor-desktop" | "unknown:<nombre>"
    pub tool: String,
    /// "sessions" | "cache" | "config" | "skills" | "plugins" | "orphaned" | "other"
    pub category: String,
    pub path: String,
    pub size_bytes: u64,
    pub item_count: u64,
    /// Seconds since the epoch; 0.0 if unknown.
    pub last_modified: f64,
    /// Seconds since the epoch when the item was created; 0.0 where the platform does not record it.
    pub created: f64,
    /// Project the item belongs to, when the tool records a working directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    pub sensitive: bool,
    pub note: String,
    pub title: String,
    /// False if it is a synthetic aggregate that cannot be moved as a single unit.
    pub actionable: bool,
    /// Token usage of the session, when the tool records it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<SessionUsage>,
}

impl Entry {
    pub fn new(id: &str, tool: &str, category: &str, path: &str) -> Self {
        Entry {
            id: id.to_string(),
            tool: tool.to_string(),
            category: category.to_string(),
            path: path.to_string(),
            size_bytes: 0,
            item_count: 0,
            last_modified: 0.0,
            created: 0.0,
            project: None,
            sensitive: false,
            note: String::new(),
            title: String::new(),
            actionable: true,
            usage: None,
        }
    }

    pub fn with_stats(mut self, size: u64, count: u64, mtime: f64) -> Self {
        self.size_bytes = size;
        self.item_count = count;
        self.last_modified = mtime;
        self
    }

    pub fn project(mut self, project: impl Into<String>) -> Self {
        self.project = Some(project.into());
        self
    }

    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.note = note.into();
        self
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    pub fn sensitive(mut self, sensitive: bool) -> Self {
        self.sensitive = sensitive;
        self
    }

    pub fn informational(mut self) -> Self {
        self.actionable = false;
        self
    }

    /// JSON consumed by the interface: the raw fields plus the derived ones
    /// (readable size, date, age in days), as in Entry.to_json().
    pub fn to_json(&self, now: f64) -> Value {
        let iso = if self.last_modified != 0.0 {
            chrono::DateTime::from_timestamp(self.last_modified as i64, 0)
                .map(|dt| dt.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "—".to_string())
        } else {
            "—".to_string()
        };
        let age_days = if self.last_modified != 0.0 {
            json!(((now - self.last_modified) / 86400.0) as i64)
        } else {
            Value::Null
        };
        let created_iso = if self.created != 0.0 {
            chrono::DateTime::from_timestamp(self.created as i64, 0)
                .map(|dt| dt.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "—".to_string())
        } else {
            "—".to_string()
        };
        let mut value = serde_json::to_value(self).unwrap_or(Value::Null);
        if let Value::Object(map) = &mut value {
            map.insert("size_human".into(), json!(human_size(self.size_bytes)));
            map.insert("last_modified_iso".into(), json!(iso));
            map.insert("created_iso".into(), json!(created_iso));
            map.insert("age_days".into(), age_days);
        }
        value
    }
}
