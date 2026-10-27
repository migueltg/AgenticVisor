# Additional capabilities identified in a local agent workspace manager

This is a feature comparison against AgenticVisor `v0.1.0-beta.1`. It records capabilities present in a separate local manager that are not covered by that release; it does not propose implementation details.

- **Codex support:** read Codex JSONL conversations, derive workspace metadata from session records, and inspect Codex configuration and instruction files. The release has no Codex adapter.
- **Hermes support:** read Hermes JSONL conversations and inspect its configuration, user instructions, and skills. The release has no Hermes adapter.
- **Per-tool runtime status:** show whether each supported agent process is running alongside conversation counts and local storage totals.
- **Project-root conversation filters:** filter sessions by their recorded working directory and group them by project-root hierarchy.
- **Conversation creation time:** show creation time, last activity, and session size together. The release exposes size and last-modified metadata, but not a separate creation timestamp.
- **Targeted Cursor conversation deletion:** remove one conversation's SQLite records while retaining unrelated conversations, after making a consistent database backup. The release treats Cursor's chat database as informational because deleting the database would lose all conversations.
- **Backup inventory:** list backups by platform with their creation time and size, then remove one backup or empty the list after confirmation. The release provides a trash and backup location, but no comparable per-backup inventory and cleanup view.

Codex and Hermes use formats and configuration layouts that differ from Claude Code and Cursor. They would need independent adapters and verified local format readers rather than generic directory classification.
