//! Tests for work plans: one inventory entry per plan file, for both tools.
//! Each test uses its own temporary home.

use std::fs;
use std::path::{Path, PathBuf};

use agenticvisor::scanner;
use agenticvisor::{adapters::claude_code, adapters::cursor_cli};

fn temp_home(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("agv-plans-{name}-{}", std::process::id()));
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
fn cursor_plan_takes_name_overview_and_task_progress() {
    let home = temp_home("cursor");
    write(
        &home.join(".cursor/plans/fix_example_1234abcd.plan.md"),
        "---\nname: Fix the example\noverview: Repair the example module\ntodos:\n  - id: a\n    status: completed\n  - id: b\n    status: pending\n  - id: c\n    status: completed\nisProject: false\n---\n\n# Body\n",
    );

    let entries = cursor_cli::scan(&home);
    let plan = entries.iter().find(|e| e.path.ends_with(".plan.md")).expect("plan entry");
    assert_eq!(plan.tool, "cursor-cli");
    assert_eq!(plan.category, "sessions");
    assert_eq!(plan.title, "Fix the example");
    assert!(plan.note.contains("Repair the example module"));
    assert!(plan.note.contains("2 of 3 tasks done"), "{}", plan.note);
    assert!(plan.project.is_none());
}

#[test]
fn claude_plan_takes_its_first_heading() {
    let home = temp_home("claude");
    write(&home.join(".claude/plans/quiet-plan.md"), "Intro line\n\n# Migration plan\n\n## Steps\n");

    let entries = claude_code::scan(&home);
    let plan = entries.iter().find(|e| e.path.ends_with("quiet-plan.md")).expect("plan entry");
    assert_eq!(plan.tool, "claude-code");
    assert_eq!(plan.title, "Migration plan");
}

#[test]
fn plans_folder_is_not_listed_as_one_entry() {
    let home = temp_home("folder");
    write(&home.join(".cursor/plans/one_0001.plan.md"), "---\nname: One\n---\n");
    write(&home.join(".cursor/plans/two_0002.plan.md"), "---\nname: Two\n---\n");

    let scan = scanner::run_scan(&home);
    let plans_folder = home.join(".cursor/plans").to_string_lossy().into_owned();
    assert!(scan.entries.iter().all(|e| e.path != plans_folder), "the folder should not be one entry");
    let plan_count = scan.entries.iter().filter(|e| e.path.contains("/plans/")).count();
    assert_eq!(plan_count, 2);
}
