//! Tests for projects: grouping, deleting a session or a project completely, and
//! moving a session to another project. Each test uses its own temporary home.

use std::fs;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

use agenticvisor::projects::{
    claude_plan_project, claude_session, delete_claude_session, delete_project, encode_folder, list, move_claude_session,
    project_path_for_folder, remember,
};
use agenticvisor::scanner::run_scan;
use serde_json::{json, Value};

const ID_A: &str = "11111111-1111-4111-8111-111111111111";
const ID_B: &str = "22222222-2222-4222-8222-222222222222";
const ID_C: &str = "33333333-3333-4333-8333-333333333333";

struct Home {
    home: PathBuf,
    data: PathBuf,
}

impl Home {
    fn new(name: &str) -> Home {
        let home = std::env::temp_dir().join(format!("agv-proj-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(&home).expect("create temp home");
        Home { data: home.join("data"), home }
    }
    fn trash(&self) -> PathBuf {
        self.data.join("trash")
    }
    fn backups(&self) -> PathBuf {
        self.data.join("backups")
    }
    /// A project directory (the "repository") that exists on disk.
    fn repo(&self, name: &str) -> PathBuf {
        let dir = self.home.join("work").join(name);
        fs::create_dir_all(&dir).expect("create repo");
        fs::write(dir.join("README.md"), "the real repository").expect("write repo file");
        dir
    }
    fn folder_of(&self, project: &Path) -> PathBuf {
        self.home.join(".claude/projects").join(encode_folder(&project.to_string_lossy()))
    }
    /// A session with its transcript, folder, session-env and file-history.
    fn session(&self, project: &Path, id: &str) -> PathBuf {
        let folder = self.folder_of(project);
        fs::create_dir_all(folder.join(id).join("subagents")).expect("create sidecar");
        fs::write(folder.join(id).join("subagents/agent.jsonl"), "{}\n").expect("write sidecar");
        let jsonl = folder.join(format!("{id}.jsonl"));
        let record = json!({"type": "user", "cwd": project.to_string_lossy(), "sessionId": id, "message": {"content": "hello"}});
        fs::write(&jsonl, format!("{record}\n")).expect("write transcript");
        fs::create_dir_all(self.home.join(".claude/session-env").join(id)).expect("session-env");
        fs::create_dir_all(self.home.join(".claude/file-history").join(id)).expect("file-history");
        fs::write(self.home.join(".claude/file-history").join(id).join("v1"), "x").expect("history file");
        jsonl
    }
    fn history(&self, lines: &[Value]) {
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        fs::write(self.home.join(".claude/history.jsonl"), text).expect("write history");
    }
    fn history_lines(&self) -> Vec<Value> {
        fs::read_to_string(self.home.join(".claude/history.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }
}

fn history_line(project: &Path, id: &str) -> Value {
    json!({"display": "a prompt", "pastedContents": {}, "timestamp": 1, "project": project.to_string_lossy(), "sessionId": id})
}

#[test]
fn folder_name_follows_claudes_rule() {
    assert_eq!(encode_folder("/Users/me/dev/my_repo"), "-Users-me-dev-my-repo");
    assert_eq!(encode_folder("/Users/me/.hidden/app"), "-Users-me--hidden-app");
}

#[test]
fn project_is_the_cwd_that_encodes_to_the_folder_name() {
    let h = Home::new("path");
    let repo = h.repo("my_repo.v2");
    let jsonl = h.session(&repo, ID_A);
    let (path, exists) = project_path_for_folder(jsonl.parent().unwrap());
    assert_eq!(path, repo.to_string_lossy());
    assert!(exists);
}

#[test]
fn deleting_a_session_removes_everything_that_belongs_to_it() {
    let h = Home::new("delete");
    let repo = h.repo("app");
    let other = h.repo("other");
    let jsonl = h.session(&repo, ID_A);
    let kept = h.session(&other, ID_B);
    h.history(&[history_line(&repo, ID_A), history_line(&repo, ID_A), history_line(&other, ID_B)]);

    let result = delete_claude_session(&h.home, &h.trash(), &h.backups(), &jsonl.to_string_lossy()).expect("delete");
    assert_eq!(result["history_removed"], 2);
    assert_eq!(result["moved"], 4, "transcript, folder, session-env and file-history");

    assert!(!jsonl.exists());
    assert!(!h.folder_of(&repo).join(ID_A).exists());
    assert!(!h.home.join(".claude/session-env").join(ID_A).exists());
    assert!(!h.home.join(".claude/file-history").join(ID_A).exists());
    assert!(h.trash().is_dir(), "moved to the trash, not deleted");
    assert!(fs::read_dir(h.backups()).unwrap().count() >= 1, "history.jsonl was backed up");

    assert!(kept.exists(), "another session is untouched");
    assert!(h.home.join(".claude/session-env").join(ID_B).exists());
    let lines = h.history_lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["sessionId"], ID_B);
}

#[test]
fn only_real_session_transcripts_can_be_deleted() {
    let h = Home::new("guard");
    let repo = h.repo("app");
    h.session(&repo, ID_A);
    let outside = h.home.join("notes.jsonl");
    fs::write(&outside, "{}").unwrap();
    for bad in [
        outside.to_string_lossy().into_owned(),
        h.folder_of(&repo).join("memory.jsonl").to_string_lossy().into_owned(),
        format!("{}/../projects/{}/{ID_A}.jsonl", h.folder_of(&repo).display(), encode_folder(&repo.to_string_lossy())),
    ] {
        assert!(claude_session(&h.home, &bad).is_err(), "{bad}");
    }
    assert!(outside.exists());
}

#[test]
fn moving_a_session_keeps_its_transcript_and_updates_its_project() {
    let h = Home::new("move");
    let from = h.repo("from");
    let to = h.repo("to");
    let jsonl = h.session(&from, ID_A);
    let before = fs::read(&jsonl).unwrap();
    h.history(&[history_line(&from, ID_A), history_line(&from, ID_B)]);

    let result = move_claude_session(&h.home, &h.backups(), &jsonl.to_string_lossy(), &to.to_string_lossy()).expect("move");
    assert_eq!(result["history_updated"], 1);

    let moved = h.folder_of(&to).join(format!("{ID_A}.jsonl"));
    assert!(moved.is_file());
    assert!(!jsonl.exists());
    assert_eq!(fs::read(&moved).unwrap(), before, "the transcript is not edited");
    assert!(h.folder_of(&to).join(ID_A).join("subagents/agent.jsonl").is_file(), "the session folder moves too");
    assert!(h.home.join(".claude/session-env").join(ID_A).exists(), "session data keyed by id stays");

    let lines = h.history_lines();
    let moved_line = lines.iter().find(|l| l["sessionId"] == ID_A).unwrap();
    assert_eq!(moved_line["project"], to.to_string_lossy().as_ref());
    let other_line = lines.iter().find(|l| l["sessionId"] == ID_B).unwrap();
    assert_eq!(other_line["project"], from.to_string_lossy().as_ref(), "other sessions keep their project");

    // The folder now belongs to the target, even though the transcript's cwd still names the old project.
    let (path, _) = project_path_for_folder(&h.folder_of(&to));
    assert_eq!(path, to.to_string_lossy());
}

#[test]
fn moving_is_refused_for_bad_targets_and_conflicts() {
    let h = Home::new("move-guard");
    let from = h.repo("from");
    let to = h.repo("to");
    let jsonl = h.session(&from, ID_A);
    let path = jsonl.to_string_lossy().into_owned();

    assert!(move_claude_session(&h.home, &h.backups(), &path, &from.to_string_lossy()).is_err(), "same project");
    assert!(move_claude_session(&h.home, &h.backups(), &path, "/definitely/not/there").is_err(), "missing target");
    assert!(move_claude_session(&h.home, &h.backups(), &path, &std::env::temp_dir().to_string_lossy()).is_err(), "outside home");
    h.session(&to, ID_A);
    assert!(move_claude_session(&h.home, &h.backups(), &path, &to.to_string_lossy()).is_err(), "id already there");
    assert!(jsonl.exists(), "nothing moved after a refusal");
}

#[test]
fn deleting_a_project_removes_all_its_data_but_never_the_repository() {
    let h = Home::new("project");
    let repo = h.repo("doomed");
    let other = h.repo("safe");
    h.session(&repo, ID_A);
    h.session(&repo, ID_B);
    let safe = h.session(&other, ID_C);
    fs::create_dir_all(h.folder_of(&repo).join("memory")).unwrap();
    fs::write(h.folder_of(&repo).join("memory/MEMORY.md"), "notes").unwrap();
    h.history(&[history_line(&repo, ID_A), history_line(&repo, ID_B), history_line(&repo, "99999999-9999-4999-8999-999999999999"), history_line(&other, ID_C)]);

    let result = delete_project(&h.home, &h.trash(), &h.backups(), &repo.to_string_lossy(), false).expect("delete project");
    assert_eq!(result["claude_sessions"], 2);
    assert_eq!(result["history_removed"], 3, "including a line whose transcript was already gone");

    assert!(!h.folder_of(&repo).exists(), "the project folder, memory included, is gone");
    assert!(!h.home.join(".claude/session-env").join(ID_A).exists());
    assert!(!h.home.join(".claude/file-history").join(ID_B).exists());
    assert!(repo.join("README.md").is_file(), "the real repository is never touched");

    assert!(safe.exists(), "another project is untouched");
    assert_eq!(h.history_lines().len(), 1);
    assert!(delete_project(&h.home, &h.trash(), &h.backups(), "/not/a/project", false).is_err());
}

#[test]
fn projects_are_listed_with_their_sessions() {
    let h = Home::new("list");
    let a = h.repo("a");
    let b = h.repo("b");
    h.session(&a, ID_A);
    h.session(&a, ID_B);
    h.session(&b, ID_C);
    let scan = run_scan(&h.home);
    let listed = list(&scan.entries);
    let items = listed["items"].as_array().unwrap();
    let project_a = items.iter().find(|i| i["project"] == a.to_string_lossy().as_ref()).expect("project a");
    assert_eq!(project_a["sessions"], 2);
    assert_eq!(project_a["name"], "a");
    assert_eq!(project_a["exists"], true);
    assert!(items.iter().any(|i| i["project"] == b.to_string_lossy().as_ref()));
}

#[test]
fn a_folder_without_a_matching_cwd_uses_the_remembered_path() {
    let h = Home::new("hint");
    let to = h.repo("empty_target");
    let folder = h.folder_of(&to);
    fs::create_dir_all(&folder).unwrap();
    let other = h.repo("elsewhere");
    fs::write(folder.join(format!("{ID_A}.jsonl")), format!("{}\n", json!({"cwd": other.to_string_lossy()}))).unwrap();
    remember(&encode_folder(&to.to_string_lossy()), &to.to_string_lossy());
    assert_eq!(project_path_for_folder(&folder).0, to.to_string_lossy());
}

#[test]
fn a_claude_plan_is_assigned_to_the_session_that_wrote_it() {
    let h = Home::new("plan");
    let repo = h.repo("planned");
    let jsonl = h.session(&repo, ID_A);
    // Skip where the platform does not report creation times.
    if fs::metadata(&jsonl).unwrap().created().is_err() {
        return;
    }
    sleep(Duration::from_millis(40));
    let plans = h.home.join(".claude/plans");
    fs::create_dir_all(&plans).unwrap();
    let plan = plans.join("calm-blue-fox.md");
    fs::write(&plan, "# Plan").unwrap();
    sleep(Duration::from_millis(40));
    let mut text = fs::read_to_string(&jsonl).unwrap();
    text.push_str(&format!("{}\n", json!({"type": "assistant", "cwd": repo.to_string_lossy(), "text": "saved to calm-blue-fox.md"})));
    fs::write(&jsonl, text).unwrap();

    let project = claude_plan_project(&h.home.join(".claude"), &plan);
    assert_eq!(project.as_deref(), Some(repo.to_string_lossy().as_ref()));
}

#[test]
fn a_project_left_with_only_its_memory_can_still_be_deleted() {
    let h = Home::new("leftover");
    let repo = h.repo("leftover");
    fs::create_dir_all(h.folder_of(&repo).join("memory")).unwrap();
    fs::write(h.folder_of(&repo).join("memory/MEMORY.md"), "notes").unwrap();
    // With no transcript to read the path from, the folder name cannot be turned back into a
    // path on every platform; the app remembers the path (as it does after moving a session).
    remember(&encode_folder(&repo.to_string_lossy()), &repo.to_string_lossy());

    let scan = run_scan(&h.home);
    assert!(list(&scan.entries)["items"].as_array().unwrap().iter().any(|i| i["project"] == repo.to_string_lossy().as_ref()));
    delete_project(&h.home, &h.trash(), &h.backups(), &repo.to_string_lossy(), false).expect("delete");
    assert!(!h.folder_of(&repo).exists());
    assert!(repo.join("README.md").is_file());
}
