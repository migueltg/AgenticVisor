# Contributing

Thanks for wanting to help. The most useful contributions right now are
verified locations and format readers for tools and operating systems the
maintainers cannot test.

## Before you start

- Run the tests: `cargo test`. They must pass. If you change the interface, also run the
  tests in `tests/ui/` (see the README); they use a fake home and never read real data.
- Check your change against real data before you open a pull request. Run
  `agenticvisor scan-json` and compare the result with what the interface
  shows.
- Never commit personal data: conversations, settings or the dashboard's own data
  folder. `.gitignore` already excludes the usual places; do not force-add them.
- Use generic examples in code, comments and tests. Do not use real project
  names, usernames, paths or conversation text.

## Adding or fixing a tool adapter

1. Find where the tool stores its data on each operating system. Write down the
   evidence (the path you checked, the operating system, the tool version).
2. Implement or update `src/adapters/<tool>.rs` with a `scan(home) -> Vec<Entry>`
   function, and register it in `src/scanner.rs`. `src/adapters/claude_code.rs` is
   the simplest reference.
3. Add tests under `tests/` that build a fake home directory with the same
   layout. Do not depend on your real files.
4. If the tool also has conversations, add a reader in `src/search/sources.rs`
   and, if needed, a branch in `src/preview.rs`.
5. Update the platform table in the README with what you verified.

## Changing the interface

`static/index.html` is a single file with no build step. Keep it that way.
Any new endpoint must:
- accept identifiers from a fixed catalogue, not arbitrary paths;
- make a backup before writing;
- report errors as `{"ok": false, "error": "..."}` with a message a user can act on.

## Pull requests

- Keep changes focused. One behaviour per pull request.
- Describe what you verified and how (operating system, sample data used).
- Include or update tests for any change to parsing, paths or writes.
