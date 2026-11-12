//! Tests for Cursor and Claude Desktop sessions: which project they belong to,
//! deleting them completely, and moving Cursor command line conversations.
//! Each test builds its own fake home folder.

use std::fs;
use std::path::{Path, PathBuf};

use agenticvisor::cursor_chats::chat_database;
use agenticvisor::cursor_sessions::{chats_folder_of, delete_conversation, ide_chat_entries, move_conversation, slug_of};
use agenticvisor::desktop_sessions::{agent_mode_entries, app_root};
use agenticvisor::projects::{delete_path, delete_project, encode_folder};
use agenticvisor::scanner::run_scan;
use rusqlite::{params, Connection};
use serde_json::{json, Value};

const CHAT: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const OTHER: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
const CLAUDE: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";

struct Home {
    home: PathBuf,
}

impl Home {
    fn new(name: &str) -> Home {
        let home = std::env::temp_dir().join(format!("agv-tools-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(&home).expect("create temp home");
        Home { home }
    }
    fn trash(&self) -> PathBuf {
        self.home.join("data/trash")
    }
    fn backups(&self) -> PathBuf {
        self.home.join("data/backups")
    }
    fn repo(&self, name: &str) -> String {
        let dir = self.home.join("work").join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("README.md"), "the real repository").unwrap();
        dir.to_string_lossy().into_owned()
    }
    fn cursor(&self) -> PathBuf {
        self.home.join(".cursor")
    }
    /// A chat made with the command line agent, and its transcript.
    fn cli_chat(&self, project: &str, id: &str) {
        let chat = self.cursor().join("chats").join(chats_folder_of(project)).join(id);
        fs::create_dir_all(&chat).unwrap();
        fs::write(chat.join("meta.json"), json!({"cwd": project, "hasConversation": true}).to_string()).unwrap();
        fs::write(chat.join("store.db"), "db").unwrap();
        let transcript = self.cursor().join("projects").join(slug_of(project)).join("agent-transcripts").join(id);
        fs::create_dir_all(&transcript).unwrap();
        fs::write(transcript.join(format!("{id}.jsonl")), "{}\n").unwrap();
    }
    /// The IDE's global database with one chat (and a second one, to check it is left alone).
    fn ide_database(&self, ids: &[&str]) {
        let db = chat_database(&self.home);
        fs::create_dir_all(db.parent().unwrap()).unwrap();
        let con = Connection::open(&db).unwrap();
        con.execute_batch("CREATE TABLE IF NOT EXISTS cursorDiskKV (key TEXT PRIMARY KEY, value BLOB);").unwrap();
        for id in ids {
            let composer = json!({"name": format!("Chat {id}"), "lastUpdatedAt": 1_700_000_000_000u64, "createdAt": 1_690_000_000_000u64});
            con.execute("INSERT INTO cursorDiskKV VALUES (?1, ?2)", params![format!("composerData:{id}"), composer.to_string()]).unwrap();
            con.execute("INSERT INTO cursorDiskKV VALUES (?1, ?2)", params![format!("bubbleId:{id}:1"), "{}"]).unwrap();
        }
    }
    /// A workspace of the IDE for `project`, listing the given chats.
    fn workspace(&self, hash: &str, project: &str, ids: &[&str]) -> PathBuf {
        let dir = self.home.join("Library/Application Support/Cursor/User/workspaceStorage").join(hash);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("workspace.json"), json!({"folder": format!("file://{project}")}).to_string()).unwrap();
        let con = Connection::open(dir.join("state.vscdb")).unwrap();
        con.execute_batch("CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);").unwrap();
        let list = json!({
            "allComposers": ids.iter().map(|i| json!({"composerId": i, "type": "head"})).collect::<Vec<_>>(),
            "selectedComposerIds": ids,
        });
        con.execute("INSERT INTO ItemTable VALUES ('composer.composerData', ?1)", params![list.to_string()]).unwrap();
        dir
    }
    fn workspace_ids(&self, dir: &Path) -> Vec<String> {
        let con = Connection::open(dir.join("state.vscdb")).unwrap();
        let text: String = con.query_row("SELECT value FROM ItemTable WHERE key = 'composer.composerData'", [], |r| r.get(0)).unwrap();
        let list: Value = serde_json::from_str(&text).unwrap();
        list["allComposers"].as_array().unwrap().iter().map(|c| c["composerId"].as_str().unwrap().to_string()).collect()
    }
    fn global_rows(&self) -> i64 {
        Connection::open(chat_database(&self.home)).unwrap().query_row("SELECT count(*) FROM cursorDiskKV", [], |r| r.get(0)).unwrap()
    }
    fn desktop_dir(&self, kind: &str) -> PathBuf {
        let dir = app_root(&self.home).join(kind).join("account").join("org");
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}

#[test]
fn chats_folder_is_the_md5_of_the_project_path() {
    assert_eq!(chats_folder_of("abc"), "900150983cd24fb0d6963f7d28e17f72");
    assert_eq!(slug_of("/Users/me/dev/my_repo.v2"), "Users-me-dev-my-repo-v2");
}

#[test]
fn moving_a_cli_conversation_moves_its_chat_and_its_transcript() {
    let h = Home::new("cursor-move");
    let from = h.repo("from_repo");
    let to = h.repo("to.repo");
    h.cli_chat(&from, CHAT);
    let chat = h.cursor().join("chats").join(chats_folder_of(&from)).join(CHAT);

    let result = move_conversation(&h.home, &chat.to_string_lossy(), &to).expect("move");
    assert_eq!(result["moved"], 2);
    assert_eq!(result["warnings"], json!([]));

    let moved_chat = h.cursor().join("chats").join(chats_folder_of(&to)).join(CHAT);
    assert!(moved_chat.join("store.db").is_file());
    assert!(!chat.exists());
    assert!(!chat.parent().unwrap().exists(), "the emptied chats folder is removed too");
    let meta: Value = serde_json::from_str(&fs::read_to_string(moved_chat.join("meta.json")).unwrap()).unwrap();
    assert_eq!(meta["cwd"], to.as_str());
    assert!(h.cursor().join("projects").join(slug_of(&to)).join("agent-transcripts").join(CHAT).is_dir());
    assert!(!h.cursor().join("projects").join(slug_of(&from)).join("agent-transcripts").join(CHAT).exists());

    // The scan now lists it under the target project, even though the slug alone cannot give the path back.
    let scan = run_scan(&h.home);
    let projects: Vec<Option<String>> = scan.entries.iter().filter(|e| e.path.ends_with(CHAT)).map(|e| e.project.clone()).collect();
    assert_eq!(projects.len(), 2, "the chat folder and the transcript");
    assert!(projects.iter().all(|p| p.as_deref() == Some(to.as_str())), "{projects:?}");
}

#[test]
fn a_conversation_made_in_the_ide_is_not_moved() {
    let h = Home::new("cursor-ide-move");
    let from = h.repo("from");
    let to = h.repo("to");
    h.cli_chat(&from, CHAT);
    h.ide_database(&[CHAT]);
    let transcript = h.cursor().join("projects").join(slug_of(&from)).join("agent-transcripts").join(CHAT);

    assert!(move_conversation(&h.home, &transcript.to_string_lossy(), &to).is_err());
    assert!(transcript.is_dir(), "nothing moved");
    let other_target = h.home.join("nowhere");
    h.cli_chat(&from, OTHER);
    let cli_only = h.cursor().join("projects").join(slug_of(&from)).join("agent-transcripts").join(OTHER);
    assert!(move_conversation(&h.home, &cli_only.to_string_lossy(), &other_target.to_string_lossy()).is_err(), "target must exist");
    assert!(move_conversation(&h.home, &cli_only.to_string_lossy(), &std::env::temp_dir().to_string_lossy()).is_err(), "outside home");
}

#[test]
fn deleting_an_ide_conversation_removes_rows_list_entries_and_folders() {
    let h = Home::new("cursor-delete");
    let repo = h.repo("app");
    h.cli_chat(&repo, CHAT);
    h.ide_database(&[CHAT, OTHER]);
    let ws = h.workspace("ws1", &repo, &[CHAT, OTHER]);

    assert!(delete_conversation(&h.home, &h.trash(), &h.backups(), CHAT, true).is_err(), "refused while Cursor runs");
    assert_eq!(h.global_rows(), 4);
    assert_eq!(h.workspace_ids(&ws).len(), 2);

    let result = delete_conversation(&h.home, &h.trash(), &h.backups(), CHAT, false).expect("delete");
    assert_eq!(result["deleted_rows"], 2);
    assert_eq!(result["workspace_lists"], 1);
    assert_eq!(h.global_rows(), 2, "the other chat's rows stay");
    assert_eq!(h.workspace_ids(&ws), vec![OTHER.to_string()]);
    assert!(!h.cursor().join("projects").join(slug_of(&repo)).join("agent-transcripts").join(CHAT).exists());
    assert!(!h.cursor().join("chats").join(chats_folder_of(&repo)).join(CHAT).exists());
    assert!(h.trash().is_dir());
    assert!(fs::read_dir(h.backups()).unwrap().count() >= 2, "the global and the workspace database were backed up");
}

#[test]
fn a_command_line_conversation_can_be_deleted_while_cursor_runs() {
    let h = Home::new("cursor-cli-delete");
    let repo = h.repo("app");
    h.cli_chat(&repo, CHAT);
    let chat = h.cursor().join("chats").join(chats_folder_of(&repo)).join(CHAT);
    let result = delete_path(&h.home, &h.trash(), &h.backups(), &chat.to_string_lossy(), true);
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["moved"], 2, "chat folder and transcript");
    assert!(!chat.exists());
    assert!(!chat.parent().unwrap().exists(), "the emptied chats folder is removed too");
}

#[test]
fn ide_chats_listed_by_a_workspace_become_entries_with_that_project() {
    let h = Home::new("ide-entries");
    let repo = h.repo("shown");
    h.cli_chat(&repo, OTHER);
    h.ide_database(&[CHAT, OTHER]);
    h.workspace("ws1", &repo, &[CHAT, OTHER]);

    let entries = ide_chat_entries(&h.home);
    assert_eq!(entries.len(), 1, "a chat that has a transcript is listed through the transcript");
    let entry = &entries[0];
    assert!(entry.path.ends_with(&format!("#{CHAT}")));
    assert_eq!(entry.project.as_deref(), Some(repo.as_str()));
    assert_eq!(entry.title, format!("Chat {CHAT}"));
    assert!(entry.created > 0.0 && entry.last_modified > entry.created);

    let result = delete_path(&h.home, &h.trash(), &h.backups(), &entry.path, false);
    assert_eq!(result["ok"], true, "{result}");
    assert!(ide_chat_entries(&h.home).is_empty());
}

#[test]
fn agent_mode_sessions_are_listed_one_by_one_and_deleted_whole() {
    let h = Home::new("agent-mode");
    let repo = h.repo("picked");
    let dir = h.desktop_dir("local-agent-mode-sessions");
    let id = "local_dddddddd-dddd-4ddd-8ddd-dddddddddddd";
    fs::create_dir_all(dir.join(id).join("outputs")).unwrap();
    fs::write(dir.join(id).join("outputs/result.txt"), "x").unwrap();
    fs::write(dir.join(format!("{id}.json")), json!({"title": "Audit", "createdAt": 1_690_000_000_000u64, "userSelectedFolders": [repo]}).to_string()).unwrap();
    let bare = "local_eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee";
    fs::write(dir.join(format!("{bare}.json")), json!({"title": "No folder", "userSelectedFolders": []}).to_string()).unwrap();

    let entries = agent_mode_entries(&app_root(&h.home).join("local-agent-mode-sessions"));
    assert_eq!(entries.len(), 2);
    let picked = entries.iter().find(|e| e.title == "Audit").unwrap();
    assert_eq!(picked.project.as_deref(), Some(repo.as_str()));
    assert!(entries.iter().find(|e| e.title == "No folder").unwrap().project.is_none());

    let result = delete_path(&h.home, &h.trash(), &h.backups(), &picked.path, false);
    assert_eq!(result["ok"], true, "{result}");
    assert!(!dir.join(id).exists() && !dir.join(format!("{id}.json")).exists(), "folder and metadata both gone");
    assert!(dir.join(format!("{bare}.json")).exists(), "the other session is untouched");
    assert!(Path::new(&repo).join("README.md").is_file());
}

#[test]
fn deleting_a_claude_session_also_removes_its_desktop_metadata() {
    let h = Home::new("desktop-meta");
    let repo = h.repo("coded");
    let folder = h.home.join(".claude/projects").join(encode_folder(&repo));
    fs::create_dir_all(&folder).unwrap();
    let jsonl = folder.join(format!("{CLAUDE}.jsonl"));
    fs::write(&jsonl, format!("{}\n", json!({"cwd": repo, "sessionId": CLAUDE}))).unwrap();
    let dir = h.desktop_dir("claude-code-sessions");
    let meta = dir.join("local_11111111-1111-4111-8111-111111111111.json");
    fs::write(&meta, json!({"cliSessionId": CLAUDE, "cwd": repo, "originCwd": repo}).to_string()).unwrap();
    let unrelated = dir.join("local_22222222-2222-4222-8222-222222222222.json");
    fs::write(&unrelated, json!({"cliSessionId": "someone-else"}).to_string()).unwrap();

    let result = delete_path(&h.home, &h.trash(), &h.backups(), &jsonl.to_string_lossy(), false);
    assert_eq!(result["ok"], true, "{result}");
    assert!(!meta.exists(), "the metadata file of that session goes with it");
    assert!(unrelated.exists());
}

#[test]
fn deleting_a_project_removes_its_desktop_metadata_and_uses_the_origin_of_a_worktree() {
    let h = Home::new("desktop-project");
    let repo = h.repo("main_repo");
    let folder = h.home.join(".claude/projects").join(encode_folder(&repo));
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join(format!("{CLAUDE}.jsonl")), format!("{}\n", json!({"cwd": repo}))).unwrap();
    let dir = h.desktop_dir("claude-code-sessions");
    // A session that ran in a worktree of the repository, whose transcript is gone.
    let worktree = format!("{repo}/.claude/worktrees/side");
    let meta = dir.join("local_33333333-3333-4333-8333-333333333333.json");
    fs::write(&meta, json!({"cliSessionId": "gone", "cwd": worktree, "originCwd": repo}).to_string()).unwrap();
    let scratch = dir.join("local_44444444-4444-4444-8444-444444444444.json");
    let scratch_cwd = app_root(&h.home).join("scratch-workspaces/x");
    fs::write(&scratch, json!({"cliSessionId": "gone2", "cwd": scratch_cwd, "originCwd": scratch_cwd}).to_string()).unwrap();

    let result = delete_project(&h.home, &h.trash(), &h.backups(), &repo, false).expect("delete project");
    assert_eq!(result["claude_sessions"], 1);
    assert!(!meta.exists(), "a worktree session belongs to the repository it started from");
    assert!(scratch.exists(), "a scratch folder inside the app is not a project");
    assert!(Path::new(&repo).join("README.md").is_file());
}
