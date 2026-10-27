//! Core tests. Each test creates its own temporary folder and never touches
//! the user's real files.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use agenticvisor::fsutil::{classify_by_name, human_size, read_frontmatter_field, resolve_dashed_path, truncate_chars};
use agenticvisor::titles::clean_title;
use agenticvisor::trash::{empty_trash, move_to_trash, trash_info};
use agenticvisor::usage::parse_session;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Unique temporary folder for each test.
fn temp_dir(name: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("aad-test-{}-{}-{}", name, std::process::id(), n));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temporary folder");
    dir
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent folder");
    }
    fs::write(path, content).expect("write file");
}

#[test]
fn human_size_rules() {
    assert_eq!(human_size(0), "0B");
    assert_eq!(human_size(512), "512B");
    assert_eq!(human_size(1024), "1.0KB");
    assert_eq!(human_size(1536), "1.5KB");
    assert_eq!(human_size(5 * 1024 * 1024), "5.0MB");
}

#[test]
fn truncation_counts_characters_not_bytes() {
    assert_eq!(truncate_chars("áéíóú", 10), "áéíóú");
    assert_eq!(truncate_chars("áéíóúñ", 4), "áéí…");
    assert_eq!(clean_title("  uno\n\ndos   tres  "), "uno dos tres");
}

#[test]
fn classification_by_name() {
    assert_eq!(classify_by_name("Cache"), ("cache", false));
    assert_eq!(classify_by_name("api-token-store"), ("config", true));
    assert_eq!(classify_by_name("chat-history"), ("sessions", false));
    assert_eq!(classify_by_name("random"), ("other", false));
}

#[cfg(unix)]
#[test]
fn dashed_folder_names_resolve_against_disk() {
    let base = temp_dir("resolve");
    let project = base.join("my-app");
    fs::create_dir_all(&project).unwrap();
    let folder = base.to_string_lossy().trim_start_matches('/').replace('/', "-") + "-my-app";
    let resolved = resolve_dashed_path(&folder, "/").expect("must resolve");
    assert_eq!(Path::new(&resolved), project.as_path());
    assert!(resolve_dashed_path("Users-nobody-zzz-does-not-exist", "/").is_none());
}

#[test]
fn frontmatter_plain_and_folded_values() {
    let dir = temp_dir("frontmatter");
    let md = dir.join("SKILL.md");
    write(&md, "---\nname: demo\ndescription: A test skill\n---\ncuerpo\n");
    assert_eq!(read_frontmatter_field(&md, "description", 200).as_deref(), Some("A test skill"));

    write(&md, "---\ndescription: >-\n  primera parte\n  segunda parte\n---\n");
    assert_eq!(read_frontmatter_field(&md, "description", 200).as_deref(), Some("primera parte segunda parte"));
    assert_eq!(read_frontmatter_field(&md, "missing", 200), None);
}

#[test]
fn streamed_responses_are_counted_once() {
    // The same response appears three times (streaming): it must be counted once.
    let dir = temp_dir("usage");
    let file = dir.join("s.jsonl");
    let line = r#"{"type":"assistant","uuid":"u1","message":{"id":"msg_1","model":"claude-x","usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":100,"cache_creation_input_tokens":7}}}"#;
    write(&file, &format!("{line}\n{line}\n{line}\n"));
    let usage = parse_session(&file).expect("must have usage");
    let tokens = usage.by_model["claude-x"];
    assert_eq!(usage.responses, 1);
    assert_eq!((tokens.input, tokens.output, tokens.cache_read, tokens.cache_write), (10, 5, 100, 7));
    assert_eq!(usage.totals().total(), 122);
}

#[test]
fn cost_state_is_read_and_files_without_usage_are_skipped() {
    let dir = temp_dir("cost");
    let with_cost = dir.join("c.jsonl");
    write(&with_cost, "{\"type\":\"cost-state\",\"totalCostUSD\":1.25}\n");
    assert_eq!(parse_session(&with_cost).expect("coste").cost_usd, Some(1.25));

    let empty = dir.join("e.jsonl");
    write(&empty, "{\"type\":\"user\",\"message\":{\"content\":\"hola\"}}\n");
    assert!(parse_session(&empty).is_none());
}

#[test]
fn trash_moves_inside_home_and_rejects_outside() {
    let home = temp_dir("trash-home");
    let data = temp_dir("trash-data").join("trash");
    let file = home.join("notes").join("a.txt");
    write(&file, "contenido");

    let ok = move_to_trash(&home, &data, &file.to_string_lossy());
    assert_eq!(ok["ok"], true);
    assert!(!file.exists(), "the original must have been moved");
    assert!(Path::new(ok["moved_to"].as_str().unwrap()).exists());

    let outside_file = temp_dir("outside").join("secreto.txt");
    write(&outside_file, "outside the home");
    let outside = move_to_trash(&home, &data, &outside_file.to_string_lossy());
    assert_eq!(outside["ok"], false);
    assert!(outside_file.exists(), "nothing outside $HOME is touched");

    let missing = move_to_trash(&home, &data, &home.join("nope").to_string_lossy());
    assert_eq!(missing["ok"], false);
}

#[test]
fn empty_trash_keeps_the_audit_log() {
    let home = temp_dir("empty-home");
    let data = temp_dir("empty-data").join("trash");
    let file = home.join("x.txt");
    write(&file, "abc");
    move_to_trash(&home, &data, &file.to_string_lossy());
    assert_eq!(trash_info(&data)["item_count"], 2, "the move and the log entry");

    let result = empty_trash(&data);
    assert_eq!(result["removed"], 1);
    assert!(data.join("audit.log").exists(), "the log is kept");
}
