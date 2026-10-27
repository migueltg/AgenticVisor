//! Tests for the capabilities added on top of 0.1.0-beta.1: Codex and Hermes
//! adapters, creation time, project on entries, process status, backup inventory
//! and deleting one Cursor conversation. Each test uses its own temporary home.

use std::fs;
use std::path::{Path, PathBuf};

use agenticvisor::adapters::{codex, hermes};
use agenticvisor::{backups, cursor_chats, runtime, scanner};
use rusqlite::Connection;

fn temp_home(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("agv-gaps-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp home");
    dir
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, content).expect("write file");
}

#[test]
fn hermes_reads_sessions_and_marks_config_sensitive() {
    let home = temp_home("hermes");
    write(
        &home.join(".hermes/sessions/20260513_154832_abc.jsonl"),
        "{\"role\":\"session_meta\",\"model\":\"m\",\"timestamp\":\"2026-05-13T15:48:45\"}\n\
         {\"role\":\"user\",\"content\":\"Hello hermes\",\"timestamp\":\"2026-05-13T15:48:46\"}\n",
    );
    write(&home.join(".hermes/config.yaml"), "api_key: secret\n");

    let entries = hermes::scan(&home);
    let session = entries.iter().find(|e| e.category == "sessions").expect("session entry");
    assert_eq!(session.tool, "hermes");
    assert_eq!(session.title, "Hello hermes");
    assert!(session.project.is_none(), "Hermes does not record a working directory");

    let config = entries.iter().find(|e| e.path.ends_with("config.yaml")).expect("config entry");
    assert_eq!(config.category, "config");
    assert!(config.sensitive);
}

#[test]
fn codex_takes_title_and_project_from_the_rollout() {
    let home = temp_home("codex");
    write(
        &home.join(".codex/sessions/2026/01/02/rollout-2026-01-02T10-00-00-abc.jsonl"),
        "{\"type\":\"session_meta\",\"payload\":{\"id\":\"abc\",\"cwd\":\"/work/app\"}}\n\
         {\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"fix the build\"}]}}\n",
    );
    write(&home.join(".codex/auth.json"), "{}");

    let entries = codex::scan(&home);
    let session = entries.iter().find(|e| e.category == "sessions").expect("session entry");
    assert_eq!(session.tool, "codex");
    assert_eq!(session.title, "fix the build");
    assert_eq!(session.project.as_deref(), Some("/work/app"));

    let auth = entries.iter().find(|e| e.path.ends_with("auth.json")).expect("auth entry");
    assert!(auth.sensitive);
}

#[test]
fn scan_fills_creation_time_and_keeps_it_before_last_change() {
    let home = temp_home("created");
    write(&home.join(".hermes/sessions/20260513_000000_x.jsonl"), "{\"role\":\"user\",\"content\":\"hi\"}\n");

    let scan = scanner::run_scan(&home);
    let entry = scan.entries.iter().find(|e| e.tool == "hermes" && e.category == "sessions").expect("hermes session");
    // Platforms without birth time report 0.0; when they report it, it cannot be after the last change.
    assert!(entry.created == 0.0 || entry.created <= entry.last_modified + 1.0);
    assert!(entry.to_json(entry.last_modified).get("created_iso").is_some());
}

#[test]
fn process_names_are_parsed_from_ps_and_tasklist() {
    let ps = "/Applications/Claude.app/Contents/MacOS/Claude\n/usr/bin/claude\n";
    assert_eq!(runtime::parse_process_names(ps), vec!["Claude", "claude"]);

    let tasklist = "\"Cursor.exe\",\"1234\",\"Console\",\"1\",\"100 K\"\n";
    assert_eq!(runtime::parse_process_names(tasklist), vec!["Cursor"]);

    let names = vec!["Cursor".to_string()];
    assert!(runtime::is_running(&names, "cursor-desktop"));
    assert!(!runtime::is_running(&names, "cursor-cli"));
    assert_eq!(runtime::status(&names)["tools"]["cursor-desktop"]["processes"], 1);
}

#[test]
fn backups_are_listed_and_moved_to_trash_not_deleted() {
    let home = temp_home("backups");
    let data = home.join("data");
    let backups_dir = data.join("backups");
    let trash_dir = data.join("trash");
    write(&backups_dir.join("20260101-000000__claude-settings.json"), "{}");
    write(&backups_dir.join("20260102-000000__cursor-argv.json"), "{}");

    let listed = backups::list(&backups_dir);
    assert_eq!(listed["count"], 2);
    assert!(listed["items"].as_array().unwrap().iter().any(|i| i["label"] == "cursor-argv.json"));

    assert!(backups::backup_path(&backups_dir, "../secret").is_err());
    assert!(backups::backup_path(&backups_dir, "missing").is_err());

    let result = backups::move_to_trash(&home, &backups_dir, &trash_dir, &["20260101-000000__claude-settings.json".to_string()]);
    assert_eq!(result["results"]["20260101-000000__claude-settings.json"]["ok"], true);
    assert!(!backups_dir.join("20260101-000000__claude-settings.json").exists());
    assert!(trash_dir.is_dir(), "the backup is kept in the trash");
    assert_eq!(backups::list(&backups_dir)["count"], 1);
}

fn chat_database_with_two_conversations(home: &Path) -> PathBuf {
    let db = cursor_chats::chat_database(home);
    fs::create_dir_all(db.parent().expect("parent")).expect("create globalStorage");
    let con = Connection::open(&db).expect("open chat db");
    con.execute_batch("CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value BLOB);").expect("schema");
    for (key, value) in [
        ("composerData:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa", "{}"),
        ("bubbleId:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa:1", "{}"),
        ("bubbleId:aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa:2", "{}"),
        ("composerData:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb", "{}"),
        ("bubbleId:bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb:1", "{}"),
    ] {
        con.execute("INSERT INTO cursorDiskKV (key, value) VALUES (?1, ?2)", (key, value)).expect("insert");
    }
    db
}

fn row_count(db: &Path) -> i64 {
    Connection::open(db).expect("open").query_row("SELECT count(*) FROM cursorDiskKV", [], |r| r.get(0)).expect("count")
}

#[test]
fn deleting_a_cursor_conversation_keeps_the_others_and_a_backup() {
    let home = temp_home("cursor-chat");
    let db = chat_database_with_two_conversations(&home);
    let backups_dir = home.join("data/backups");
    let key = format!("{}#aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa", db.display());

    let result = cursor_chats::delete_chat(&home, &backups_dir, &key, false).expect("delete");
    assert_eq!(result["deleted_rows"], 3);
    assert_eq!(row_count(&db), 2, "only the other conversation is left");

    let backup = PathBuf::from(result["backup"].as_str().expect("backup path"));
    assert!(backup.is_file());
    assert_eq!(row_count(&backup), 5, "the backup holds the full database from before the change");
}

#[test]
fn cursor_conversation_is_not_deleted_while_cursor_runs_or_for_foreign_paths() {
    let home = temp_home("cursor-chat-guard");
    let db = chat_database_with_two_conversations(&home);
    let backups_dir = home.join("data/backups");
    let key = format!("{}#aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa", db.display());

    assert!(cursor_chats::delete_chat(&home, &backups_dir, &key, true).is_err());
    assert_eq!(row_count(&db), 5, "nothing changed while Cursor runs");

    let foreign = format!("/tmp/elsewhere.vscdb#aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa");
    assert!(cursor_chats::delete_chat(&home, &backups_dir, &foreign, false).is_err());
    assert!(cursor_chats::delete_chat(&home, &backups_dir, &format!("{}#not-an-id", db.display()), false).is_err());
    assert_eq!(row_count(&db), 5);
}

#[test]
fn session_estimate_prices_each_response_with_its_cache_split() {
    let home = temp_home("pricing");
    let file = home.join("session.jsonl");
    // One Opus 5 response: 1M input, 1M output, 1M cache writes of which 1M were 1-hour writes.
    write(
        &file,
        "{\"type\":\"assistant\",\"uuid\":\"u1\",\"message\":{\"id\":\"m1\",\"model\":\"claude-opus-5\",\"usage\":{\"input_tokens\":1000000,\"output_tokens\":1000000,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":1000000,\"cache_creation\":{\"ephemeral_1h_input_tokens\":1000000}}}}\n\
         {\"type\":\"assistant\",\"uuid\":\"u2\",\"message\":{\"id\":\"m1\",\"model\":\"claude-opus-5\",\"usage\":{\"input_tokens\":1000000,\"output_tokens\":1000000,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":1000000,\"cache_creation\":{\"ephemeral_1h_input_tokens\":1000000}}}}\n\
         {\"type\":\"assistant\",\"uuid\":\"u3\",\"message\":{\"id\":\"m2\",\"model\":\"<synthetic>\",\"usage\":{\"input_tokens\":0,\"output_tokens\":0}}}\n",
    );
    let usage = agenticvisor::usage::parse_session(&file).expect("usage");
    // The repeated line for m1 is counted once.
    assert_eq!(usage.responses, 2);
    // Opus 5: input 5 + output 25 + 1-hour cache write 10, all per million tokens.
    assert!((usage.estimated_cost_usd - 40.0).abs() < 1e-9, "{}", usage.estimated_cost_usd);
    assert_eq!(usage.unpriced_responses, 0, "a response without tokens is not counted as unpriced");
}
