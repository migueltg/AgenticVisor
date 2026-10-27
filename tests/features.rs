//! Feature tests: skills, plugins, configuration, artifacts,
//! search and preview. Each test uses its own temporary home.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use agenticvisor::artifacts::{claude_artifacts, cursor_canvases, read_canvas};
use agenticvisor::config;
use agenticvisor::extensions::{inventory, read_skill, set_plugin_enabled, set_skill_disabled, write_skill};
use agenticvisor::preview::preview;
use agenticvisor::search::index::{search, stats, sync};
use agenticvisor::search::sources::SessionSource;
use serde_json::Value;

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct FakeHome {
    home: PathBuf,
    backups: PathBuf,
}

impl FakeHome {
    fn new(name: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!("aad-feat-{}-{}-{}", name, std::process::id(), n));
        let _ = fs::remove_dir_all(&root);
        let home = root.join("home");
        fs::create_dir_all(&home).expect("casa temporal");
        FakeHome { home, backups: root.join("data").join("backups") }
    }

    fn write(&self, rel: &str, content: &str) -> PathBuf {
        let path = self.home.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("folder");
        fs::write(&path, content).expect("escribir");
        path
    }
}

fn mtime_ns(path: &Path) -> u64 {
    fs::metadata(path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}

// ---------- Skills and plugins ----------

fn claude_with_plugin(fh: &FakeHome) {
    let plug = fh.home.join(".claude/plugins/cache/mk/pa/1.0.0");
    fs::create_dir_all(plug.join(".claude-plugin")).unwrap();
    fs::write(plug.join(".claude-plugin/plugin.json"), r#"{"name":"pa","version":"1.0.0"}"#).unwrap();
    fh.write(
        ".claude/plugins/installed_plugins.json",
        &serde_json::json!({"version": 2, "plugins": {"pa@mk": [{"installPath": plug, "version": "1.0.0"}]}}).to_string(),
    );
    fh.write(".claude/settings.json", r#"{"model":"x","enabledPlugins":{}}"#);
    fh.write(".claude/skills/mine/SKILL.md", "---\ndescription: Mine\n---\nv1\n");
    fs::create_dir_all(fh.home.join(".claude/skills/.trash")).unwrap();
}

#[test]
fn inventory_lists_plugin_and_skill_without_hidden_folders() {
    let fh = FakeHome::new("inv");
    claude_with_plugin(&fh);
    let inv = inventory(&fh.home);
    assert_eq!(inv["claude_plugins"][0]["enabled"], Value::Null);
    assert_eq!(inv["claude_skills"].as_array().unwrap().len(), 1);
    assert_eq!(inv["claude_skills"][0]["name"], "mine");
}

#[test]
fn plugin_toggle_keeps_other_keys_and_backs_up() {
    let fh = FakeHome::new("plugin");
    claude_with_plugin(&fh);
    set_plugin_enabled(&fh.home, "pa@mk", true, &fh.backups).unwrap();
    let data: Value = serde_json::from_str(&fs::read_to_string(fh.home.join(".claude/settings.json")).unwrap()).unwrap();
    assert_eq!(data["enabledPlugins"]["pa@mk"], true);
    assert_eq!(data["model"], "x");
    assert_eq!(fs::read_dir(&fh.backups).unwrap().count(), 1);
    assert!(set_plugin_enabled(&fh.home, "nope@x", true, &fh.backups).is_err());
}

#[test]
fn skill_disable_and_enable_moves_the_folder() {
    let fh = FakeHome::new("skillmove");
    claude_with_plugin(&fh);
    set_skill_disabled(&fh.home, "mine", true).unwrap();
    assert!(fh.home.join(".claude/skills-disabled/mine/SKILL.md").exists());
    set_skill_disabled(&fh.home, "mine", false).unwrap();
    assert!(fh.home.join(".claude/skills/mine/SKILL.md").exists());
}

#[cfg(unix)]
#[test]
fn shared_symlink_is_unlinked_and_target_untouched() {
    let fh = FakeHome::new("symlink");
    claude_with_plugin(&fh);
    let shared = fh.home.join(".agents/skills/shared");
    fs::create_dir_all(&shared).unwrap();
    fs::write(shared.join("SKILL.md"), "Compartida\n").unwrap();
    std::os::unix::fs::symlink(&shared, fh.home.join(".claude/skills/shared")).unwrap();
    set_skill_disabled(&fh.home, "shared", true).unwrap();
    assert!(fs::symlink_metadata(fh.home.join(".claude/skills-disabled/shared")).unwrap().file_type().is_symlink());
    assert_eq!(fs::read_to_string(shared.join("SKILL.md")).unwrap(), "Compartida\n");
}

#[test]
fn invalid_skill_names_are_rejected() {
    let fh = FakeHome::new("badname");
    claude_with_plugin(&fh);
    for bad in ["../x", ".trash", "a/b", ""] {
        assert!(set_skill_disabled(&fh.home, bad, true).is_err(), "{bad}");
    }
}

#[test]
fn skill_editor_only_opens_skill_md_and_detects_external_changes() {
    let fh = FakeHome::new("skilledit");
    claude_with_plugin(&fh);
    let md = fh.home.join(".claude/skills/mine/SKILL.md");
    let current = read_skill(&fh.home, &md.to_string_lossy()).unwrap();
    assert!(current["content"].as_str().unwrap().starts_with("---"));
    assert!(read_skill(&fh.home, &md.parent().unwrap().to_string_lossy()).is_err());
    assert!(read_skill(&fh.home, &fh.home.join(".ssh/SKILL.md").to_string_lossy()).is_err());

    let expected = current["mtime_ns"].as_u64().unwrap();
    write_skill(&fh.home, &md.to_string_lossy(), "v2\n", expected, &fh.backups).unwrap();
    assert!(write_skill(&fh.home, &md.to_string_lossy(), "v3\n", expected, &fh.backups).is_err());
    assert_eq!(mtime_ns(&md) > 0, true);
}

// ---------- Configuration ----------

fn settings(fh: &FakeHome, body: &str) -> PathBuf {
    fh.write(".claude/settings.json", body)
}

#[test]
fn clean_json_is_editable_and_writable_with_backup() {
    let fh = FakeHome::new("cfgok");
    settings(&fh, "{\n  \"model\": \"haiku\",\n  \"hooks\": {}\n}\n");
    let current = config::read(&fh.home, "claude-settings").unwrap();
    assert_eq!(current["editable"], true);
    let new = "{\n  \"model\": \"opus\",\n  \"hooks\": {}\n}\n";
    config::write(&fh.home, "claude-settings", new, current["mtime_ns"].as_u64().unwrap(), &fh.backups).unwrap();
    let data: Value = serde_json::from_str(&fs::read_to_string(fh.home.join(".claude/settings.json")).unwrap()).unwrap();
    assert_eq!(data["model"], "opus");
    assert_eq!(fs::read_dir(&fh.backups).unwrap().count(), 1);
}

#[test]
fn secret_makes_file_read_only_and_masks_the_value() {
    let fh = FakeHome::new("cfgsecret");
    settings(&fh, r#"{"env":{"GITHUB_TOKEN":"ghp_abcdefghijklmnopqrstuvwx"}}"#);
    let current = config::read(&fh.home, "claude-settings").unwrap();
    assert_eq!(current["editable"], false);
    let content = current["content"].as_str().unwrap();
    assert!(content.contains("••••"));
    assert!(!content.contains("ghp_abcdef"));
}

#[test]
fn writes_are_validated() {
    let fh = FakeHome::new("cfgvalid");
    settings(&fh, "{\"model\":\"haiku\"}");
    let current = config::read(&fh.home, "claude-settings").unwrap();
    let mtime = current["mtime_ns"].as_u64().unwrap();
    assert!(config::write(&fh.home, "claude-settings", "{oops", mtime, &fh.backups).is_err());
    assert!(config::write(&fh.home, "claude-settings", r#"{"env":{"API_TOKEN":"abc123"}}"#, mtime, &fh.backups).is_err());
    assert!(config::write(&fh.home, "claude-settings", "{}", 1, &fh.backups).is_err(), "outdated version");
}

#[test]
fn unknown_ids_and_paths_are_rejected() {
    let fh = FakeHome::new("cfgid");
    settings(&fh, "{}");
    for bad in ["../etc/passwd", "/etc/hosts", ""] {
        assert!(config::read(&fh.home, bad).is_err(), "{bad}");
    }
}

#[test]
fn read_only_catalog_entries_are_never_writable() {
    let fh = FakeHome::new("cfgro");
    fh.write(".claude.json", r#"{"projects":{}}"#);
    let current = config::read(&fh.home, "claude-state").unwrap();
    assert_eq!(current["editable"], false);
    assert!(config::write(&fh.home, "claude-state", "{}", mtime_ns(&fh.home.join(".claude.json")), &fh.backups).is_err());
}

#[test]
fn list_only_shows_existing_files_and_reports_format() {
    let fh = FakeHome::new("cfglist");
    settings(&fh, "{}");
    fh.write(".claude/CLAUDE.md", "# Notas\n");
    let items = config::list(&fh.home);
    let ids: Vec<&str> = items.iter().map(|i| i["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["claude-settings", "claude-memory"]);
    assert_eq!(config::read(&fh.home, "claude-memory").unwrap()["format"], "text");
}

// ---------- Artefactos ----------

#[test]
fn canvases_are_listed_and_read_within_scope() {
    let fh = FakeHome::new("canvas");
    let canvases = fh.home.join(".cursor/projects/Users-x-proj/canvases");
    fs::create_dir_all(canvases.join("node_modules")).unwrap();
    fs::write(canvases.join("demo.canvas.tsx"), "export default 1;\n").unwrap();
    let items = cursor_canvases(&fh.home);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["name"], "demo");
    assert!(read_canvas(&fh.home, items[0]["path"].as_str().unwrap()).unwrap()["content"].as_str().unwrap().contains("export"));
    assert!(read_canvas(&fh.home, &fh.home.join(".ssh/config").to_string_lossy()).is_err());
}

#[test]
fn claude_publications_need_a_real_publish_result() {
    let fh = FakeHome::new("artifact");
    let call = r#"{"type":"assistant","timestamp":"2026-10-23T10:00:00.000Z","cwd":"/x/proj","message":{"content":[{"type":"tool_use","id":"t1","name":"Artifact","input":{"action":"publish","file_path":"/nope/a.html","title":"Plan demo"}}]}}"#;
    let result = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"Published /nope/a.html at https://claude.ai/artifact/abc123 (Version 1)"}]}}"#;
    let mention = r#"{"type":"user","message":{"content":"Look https://claude.ai/artifact/zzz this is not a publication"}}"#;
    fh.write(".claude/projects/-x/s.jsonl", &format!("{call}\n{result}\n{mention}\n"));
    let items = claude_artifacts(&fh.home);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["url"], "https://claude.ai/artifact/abc123");
    assert_eq!(items[0]["title"], "Plan demo");
    assert_eq!(items[0]["project"], "/x/proj");
}

// ---------- Buscador ----------

fn source(key: &str, version: &str, messages: Vec<(String, String)>) -> SessionSource {
    SessionSource {
        key: key.into(),
        tool: "claude-code".into(),
        path: format!("/x/{key}"),
        title: "Unrelated".into(),
        version: version.into(),
        read: Box::new(move || messages.clone()),
    }
}

#[test]
fn sync_is_incremental_and_search_finds_substrings() {
    let fh = FakeHome::new("index");
    let db = fh.home.join("index.sqlite");
    let msgs = vec![
        ("user".to_string(), "I need to review the Proxmox VPN".to_string()),
        ("tool".to_string(), "tool output with TEST_TOKEN".to_string()),
    ];
    let first = sync(&db, vec![source("s1", "v1", msgs.clone())], "full").unwrap();
    assert_eq!(first.indexed, 1);
    assert_eq!(search(&db, "roxmo", 50).unwrap()[0].key, "s1");
    assert_eq!(search(&db, "TEST_TOKEN", 50).unwrap()[0].key, "s1");

    let again = sync(&db, vec![source("s1", "v1", msgs)], "full").unwrap();
    assert_eq!(again.unchanged, 1, "same version: not read again");
    assert_eq!(again.indexed, 0);
}

#[test]
fn tool_outputs_follow_the_mode_and_short_queries_return_nothing() {
    let fh = FakeHome::new("indexmode");
    let db = fh.home.join("index.sqlite");
    let src = || source("s1", "v1", vec![("tool".to_string(), "SALIDA_ESPECIAL_123".to_string())]);
    sync(&db, vec![src()], "none").unwrap();
    assert!(search(&db, "SALIDA_ESPECIAL", 50).unwrap().is_empty());
    sync(&db, vec![src()], "full").unwrap();
    assert_eq!(search(&db, "SALIDA_ESPECIAL", 50).unwrap()[0].key, "s1");
    assert_eq!(stats(&db)["tool_mode"], "full");
    assert!(search(&db, "ab", 50).unwrap().is_empty());
}

// ---------- Vista previa ----------

/// A file that exists outside any test home.
fn temp_dir_for_outside() -> String {
    let dir = std::env::temp_dir().join(format!("aad-outside-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("external folder");
    let file = dir.join("fuera.txt");
    fs::write(&file, "x").expect("external file");
    file.to_string_lossy().into_owned()
}

fn claude_transcript(fh: &FakeHome) -> PathBuf {
    let mut lines = Vec::new();
    for i in 0..45 {
        lines.push(format!(r#"{{"type":"user","message":{{"content":"pregunta {i}"}}}}"#));
    }
    lines.push(
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"respuesta"},{"type":"thinking","thinking":"pensando"},{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#
            .to_string(),
    );
    fh.write(".claude/projects/-x/abc.jsonl", &(lines.join("\n") + "\n"))
}

#[test]
fn preview_pages_a_claude_transcript() {
    let fh = FakeHome::new("prev");
    let path = claude_transcript(&fh);
    let key = path.to_string_lossy().into_owned();
    let first = preview(&fh.home, &key, "0", 40).unwrap();
    assert_eq!(first["messages"].as_array().unwrap().len(), 40);
    assert_eq!(first["has_more"], true);
    let second = preview(&fh.home, &key, "40", 40).unwrap();
    assert_eq!(second["messages"].as_array().unwrap().len(), 8);
    assert_eq!(second["has_more"], false);
}

#[test]
fn preview_removes_cursor_tags_only_for_display() {
    let fh = FakeHome::new("prevcursor");
    let conv = fh.home.join(".cursor/projects/p/agent-transcripts/u1");
    fs::create_dir_all(&conv).unwrap();
    let line = r#"{"role":"user","message":{"content":[{"type":"text","text":"<timestamp>Monday</timestamp>\n<user_query>\nhola cursor\n</user_query>"}]}}"#;
    fs::write(conv.join("u1.jsonl"), format!("{line}\n")).unwrap();
    let out = preview(&fh.home, &conv.to_string_lossy(), "0", 40).unwrap();
    assert_eq!(out["messages"][0]["text"], "hola cursor");
}

#[test]
fn preview_reads_a_cursor_chat_database() {
    let fh = FakeHome::new("prevchat");
    let chat = fh.home.join(".cursor/chats/h/c1");
    fs::create_dir_all(&chat).unwrap();
    let con = rusqlite::Connection::open(chat.join("store.db")).unwrap();
    con.execute_batch("CREATE TABLE blobs (id INTEGER PRIMARY KEY, data TEXT);").unwrap();
    con.execute("INSERT INTO blobs(data) VALUES (?1)", [r#"{"role":"system","content":"prompt"}"#]).unwrap();
    con.execute("INSERT INTO blobs(data) VALUES (?1)", [r#"{"role":"assistant","content":"hello human"}"#]).unwrap();
    drop(con);
    let out = preview(&fh.home, &chat.to_string_lossy(), "0", 40).unwrap();
    assert_eq!(out["messages"].as_array().unwrap().len(), 1);
    assert_eq!(out["messages"][0]["text"], "hello human");
}

#[test]
fn preview_reads_plans_and_rejects_outside_roots_and_projects() {
    let fh = FakeHome::new("prevplan");
    let plan = fh.write(".claude/plans/plan.md", "# Plan\nStep one\n");
    let out = preview(&fh.home, &plan.to_string_lossy(), "0", 40).unwrap();
    assert!(out["messages"][0]["text"].as_str().unwrap().contains("Step one"));

    fh.write("secret.txt", "x");
    let outside = temp_dir_for_outside();
    for key in [fh.home.join("secret.txt").to_string_lossy().into_owned(), outside, format!("{}/.claude/projects/../../secret.txt", fh.home.display())] {
        assert!(preview(&fh.home, &key, "0", 40).is_err(), "{key}");
    }
    let project = fh.home.join(".cursor/projects/Users-x-proj");
    fs::create_dir_all(&project).unwrap();
    assert!(preview(&fh.home, &project.to_string_lossy(), "0", 40).is_err());
}

#[test]
fn bad_offset_falls_back_to_first_page() {
    let fh = FakeHome::new("prevoff");
    let plan = fh.write(".claude/plans/plan.md", "# Plan\n");
    assert_eq!(preview(&fh.home, &plan.to_string_lossy(), "abc", 40).unwrap()["offset"], 0);
}
