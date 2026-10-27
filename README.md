# AgenticVisor

[![CI](https://github.com/migueltg/AgenticVisor/actions/workflows/ci.yml/badge.svg)](https://github.com/migueltg/AgenticVisor/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/migueltg/AgenticVisor?include_prereleases&label=release)](https://github.com/migueltg/AgenticVisor/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A local dashboard to see, search, clean up and configure what AI coding tools
store on your computer: Claude Code, Claude Desktop, Cursor and anything else
it can detect.

AgenticVisor is a single native binary with the interface built in. It runs
entirely on your machine and never sends your data anywhere.

> **Status: beta (0.1.0-beta.2).** Tested on macOS (Apple Silicon). The Linux and
> Windows builds are produced automatically, but their tool locations have not
> been verified yet. See [Platform support](#platform-support).

## Download

Go to the [Releases](https://github.com/migueltg/AgenticVisor/releases) page and
download the file for your system:

| System | File |
|--------|------|
| macOS (Apple Silicon) | `agenticvisor-vX.Y.Z-macos-arm64.tar.gz` |
| Linux (x86-64) | `agenticvisor-vX.Y.Z-linux-x64.tar.gz` |
| Windows (x86-64) | `agenticvisor-vX.Y.Z-windows-x64.zip` |

Check the file against `SHA256SUMS.txt` before running it. Builds are not signed
yet, so macOS and Windows may warn the first time you run them.

## Quick start

From a release, unpack the file and run the `agenticvisor` binary in it.

Or build from source (Rust toolchain required, see <https://rustup.rs>):

```bash
cargo build --release
./target/release/agenticvisor
```

The dashboard opens at <http://127.0.0.1:8756> (the browser opens by itself).
The server listens only on `127.0.0.1`, so other devices on your network cannot
reach it.

Options: `--port N`, `--no-browser`, `--version`. Commands: `scan-json` and
`usage-json` print the scan and the token usage as JSON, which is useful for scripts.

## What it does

**Overview.** How much space each tool uses, grouped by tool and category, and
how much is safe to clean. It also shows which agents are running right now and
lists the backups the dashboard made, so you can move them to the trash.

**Per agent (Claude, Cursor).**
- **Sessions**: conversations, with a readable preview (JSONL, chat databases,
  plans) and paging. A Cursor Desktop conversation can be deleted from its
  preview: only that conversation is removed, after a full copy of Cursor's chat
  database is saved, and only while Cursor is closed.
- **Cache**: data the tool regenerates by itself, the safest things to delete.
- **Orphans**: leftovers from projects that no longer exist on disk.
- **Configuration**: settings files, with secret values masked. Files without
  credentials can be edited with a JSON-aware editor, a validation step and a
  backup before every save.
- **Skills**: enable or disable Claude Code skills, and edit their `SKILL.md`.
- **Plugins**: enable or disable Claude Code plugins (a confirmed change to
  `settings.json`).
- **Artifacts and canvases**: publications registered in transcripts and Cursor
  canvases, listed locally.

**Codex and Hermes.** Sessions, configuration files and skills of the Codex CLI
and the Hermes agent. The Hermes reader is checked against a real installation.
The Codex reader follows the documented format but has not been checked against
real Codex sessions yet, so its titles and projects are best effort.

**Search inside conversations.** A local full-text index (SQLite FTS5 with
trigrams) finds any substring in the text of your sessions, including titles
that do not describe the content. Tool outputs are optional: off by default.
The first index build takes several minutes on a large history.

**Token usage.** For Claude Code and Claude Desktop: tokens per model (input,
output, cache reads and writes), the sessions that consume the most, and the
cost when the tool recorded it. Each response is counted once, even though the
transcript repeats it while streaming. Cursor is not included: its chats do not
store reliable token counters.

**Everything, in one table.** Every detected item, with filters, sorting and
expandable folders. Each row shows when it was created and when it changed, and
a project filter narrows entries to the working directory they were recorded in.

## Privacy and safety

- **No network access.** The program makes no outgoing requests. The editor
  library is embedded in the binary.
- **Read-only by default.** Nothing is deleted or changed unless you press a
  button and confirm it.
- **Nothing is deleted directly.** "Move to trash" moves items to the program's
  trash folder, where you can recover them. Emptying the trash is a separate
  action with its own confirmation. Backups are moved to the trash the same way.
  The one exception is deleting a Cursor Desktop conversation: only its rows are
  removed from Cursor's database, after a full copy of the database is saved to
  the data folder, and the change is refused while Cursor runs.
- **Backups before every write.** Settings, skills and configuration files are
  copied to the data folder before they change, and every plugin change is
  logged there.
- **Secrets are masked.** Values under keys such as `token`, `secret`,
  `password` or `authorization`, and values that look like API keys, are shown as
  `••••`. A file that contains credentials becomes read-only automatically.
- **Only known locations.** The API accepts identifiers and session keys from a
  fixed catalogue, never arbitrary paths. The local server listens only on
  `127.0.0.1`.
- **Your data stays local.** The search index, backups, change log and trash live
  in the data folder on your machine. They are never part of the project or of
  a release.

Where the data folder is:

- macOS: `~/Library/Application Support/agenticvisor`
- Linux: `~/.local/share/agenticvisor`
- Windows: `%LOCALAPPDATA%\agenticvisor`

Set `AGENTICVISOR_DATA` to use another folder.

## How it works

```
src/main.rs            command line and startup
src/server.rs          local HTTP server and API
src/scanner.rs         runs every adapter and builds the table
src/adapters/          one module per tool: where its data lives and what it is
src/search/            search index (SQLite FTS5) and session readers
src/preview.rs         paged, readable view of a conversation
src/extensions.rs      skills and plugins
src/config.rs          configuration catalogue, masking and safe writes
src/artifacts.rs       Claude publications and Cursor canvases
src/usage.rs           token usage per session
src/trash.rs           trash and emptying it
src/backups.rs         backup inventory, moved to the trash
src/cursor_chats.rs    deletes one Cursor Desktop conversation
src/runtime.rs         which agent programs are running
static/                the interface (one HTML file, no build step) and the editor
```

Each adapter (`src/adapters/<tool>.rs`) exposes a `scan` function that returns
the entries for its tool. `src/adapters/claude_code.rs` is the simplest one to
read first. To add a tool, write an adapter and register it in `src/scanner.rs`.

## Tests

```bash
cargo test
```

The tests use temporary home directories and never touch your real files. The
same tests run on Linux, macOS and Windows in continuous integration.

## Platform support

| Platform | Status |
|----------|--------|
| macOS (Apple Silicon) | Tested. Paths for Claude Code, Claude Desktop and Cursor are verified. |
| macOS (Intel) | Build from source. Not tested. |
| Linux (x86-64) | Built and tested by CI. Tool locations not verified. |
| Windows (x86-64) | Built and tested by CI. Tool locations not verified. |
| Codex CLI | Reader written from the documented format, not yet checked against real sessions. |
| Hermes agent | Checked against a real installation on macOS. |

Tool data formats are internal to each tool and can change between versions.
When a format changes, the affected reader needs updating; see
[CONTRIBUTING.md](CONTRIBUTING.md).

## Roadmap

- Verified tool locations for Linux and Windows, with help from people who use
  those systems.
- Signed builds for macOS and Windows.

## Contributing

Contributions are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md) first, and the
[Code of Conduct](CODE_OF_CONDUCT.md). To report a security problem, follow
[SECURITY.md](SECURITY.md) instead of opening a public issue.

See [CHANGELOG.md](CHANGELOG.md) for what changed in each version.

## License

[MIT](LICENSE)
