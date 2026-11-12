"""Interface tests for projects: listing, grouping, moving a session, deleting a
session completely and deleting a project. Run `cargo build` first.

    python3 tests/ui/projects_ui.py

Needs Playwright for Python and Google Chrome. All data comes from a fake home.
"""
import json
import os
import sys
import tempfile

sys.path.insert(0, os.path.dirname(__file__))
from playwright.sync_api import sync_playwright  # noqa: E402

import harness  # noqa: E402
from fixture import IDS, Fixture  # noqa: E402

results, errors = [], []


def check(name, condition, extra=""):
    results.append((name, bool(condition), extra))


def main():
    fx = Fixture(tempfile.mkdtemp(prefix="agv-ui-")).build()
    bridge = harness.Bridge(fx.home, fx.data)
    api = lambda target: bridge.call("GET", target)["body"]

    with sync_playwright() as p:
        browser = p.chromium.launch(channel="chrome")
        page = browser.new_page(viewport={"width": 1366, "height": 880})
        harness.install(page, bridge)
        page.on("console", lambda m: errors.append(m.text) if m.type == "error" else None)
        page.on("pageerror", lambda e: errors.append(str(e)))

        # Projects view lists both repositories
        page.goto(harness.BASE + "/#projects")
        page.wait_for_selector("#projectsBody [data-project-show]")
        text = page.inner_text("#projectsBody")
        check("projects view lists both repositories", "alpha" in text and "beta" in text)
        check("menu says Overview, not Resumen", "Overview" in page.inner_text(".nav-sections") and "Resumen" not in page.inner_text(".nav-sections"))

        # Show + group by project
        page.click(f'[data-project-show="{fx.alpha}"]')
        page.wait_for_selector("#tbody tr")
        check("Show opens the list filtered to the project", page.locator("#tbody tr").count() >= 2)
        page.select_option("#projectFilter", "")
        page.check("#groupByProject")
        page.wait_for_selector(".group-row")
        headers = page.locator(".group-row").all_inner_texts()
        # Alpha and beta, plus the entries that belong to no project (session data, file history).
        check("group by project shows a header per project and one for no project",
              len(headers) == 3 and any("alpha" in h for h in headers) and any("beta" in h for h in headers) and any("No project" in h for h in headers), str(headers))
        page.uncheck("#groupByProject")

        # Move a session from alpha to beta
        row = page.locator(f'#tbody tr:has(.path:text("{IDS["a2"]}"))')
        row.locator("[data-move]").click()
        page.wait_for_selector("#moveDialog[open]")
        check("move dialog does not offer the session's own project", fx.alpha not in page.locator("#moveSelect").inner_html())
        page.select_option("#moveSelect", fx.beta)
        page.click("#moveConfirm")
        page.wait_for_function("!document.getElementById('moveDialog').open")
        claude_sessions = lambda project: sum(
            1 for e in api("/api/scan")["entries"]
            if e["tool"] == "claude-code" and e["path"].endswith(".jsonl") and e.get("project") == project)
        check("the session now belongs to beta", claude_sessions(fx.beta) == 2 and claude_sessions(fx.alpha) == 1)
        check("its transcript is in beta's folder", os.path.isfile(fx.transcript(fx.beta, "a2")) and not os.path.exists(fx.transcript(fx.alpha, "a2")))

        # Move a Cursor command line conversation from alpha to beta
        page.goto(harness.BASE + "/#table/all:")
        page.wait_for_selector("#tbody tr")
        cursor_row = page.locator(f'#tbody tr:has(.path:text("{IDS["cursor"]}"))').first
        check("Cursor conversations offer Move", cursor_row.locator("[data-move]").count() == 1)
        cursor_row.locator("[data-move]").click()
        page.wait_for_selector("#moveDialog[open]")
        page.select_option("#moveSelect", fx.beta)
        page.click("#moveConfirm")
        page.wait_for_function("!document.getElementById('moveDialog').open")
        check("its chat folder and transcript are under beta, with the cwd updated",
              os.path.isdir(fx.cursor_chat(fx.beta)) and os.path.isdir(fx.cursor_transcript(fx.beta))
              and not os.path.exists(fx.cursor_chat(fx.alpha)) and not os.path.exists(fx.cursor_transcript(fx.alpha))
              and json.load(open(os.path.join(fx.cursor_chat(fx.beta), "meta.json")))["cwd"] == fx.beta)

        # Delete a session with the row button: everything that belongs to it goes
        page.goto(harness.BASE + "/#table/all:")
        page.wait_for_selector("#tbody tr")
        page.locator(f'#tbody tr:has(.path:text("{IDS["a1"]}")) [data-single-trash]').click()
        page.click("#confirmTrash")
        page.wait_for_function("!document.getElementById('confirmDialog').open")
        page.wait_for_timeout(500)
        sid = IDS["a1"]
        gone = not any(os.path.exists(p) for p in (
            fx.transcript(fx.alpha, "a1"),
            os.path.join(fx.folder(fx.alpha), sid),
            os.path.join(fx.home, ".claude", "session-env", sid),
            os.path.join(fx.home, ".claude", "file-history", sid),
        ))
        check("deleting a session removes transcript, folder, session data and file history", gone)
        check("and its history lines", sid not in fx.history_text() and IDS["b1"] in fx.history_text())

        # Delete the whole beta project
        page.goto(harness.BASE + "/#projects")
        page.wait_for_selector("#projectsBody tbody tr")
        page.locator(f'[data-project-delete="{fx.beta}"]').click()
        page.wait_for_selector("#actionDialog[open]")
        check("project delete warns that the repository is not touched", "is not touched" in page.inner_text("#actionNote"))
        page.click("#actionConfirm")
        page.wait_for_function("document.getElementById('flash').textContent.includes('Moved')")
        check("the project's data is gone, Cursor conversation included",
              not os.path.exists(fx.folder(fx.beta)) and not os.path.exists(fx.cursor_chat(fx.beta)) and not os.path.exists(fx.cursor_transcript(fx.beta)))
        check("no history line of the project is left", IDS["b1"] not in fx.history_text() and IDS["a2"] not in fx.history_text())
        check("the repositories on disk are untouched",
              all(os.path.isfile(os.path.join(r, "README.md")) for r in (fx.alpha, fx.beta)))
        check("a project left with no data disappears from the list", fx.beta not in {pr["project"] for pr in api("/api/projects")["items"]})

        check("no console errors", not errors, str(errors[:3]))
        browser.close()
    bridge.close()

    for name, good, extra in results:
        print("PASS" if good else "FAIL", name, extra)
    passed = sum(1 for r in results if r[1])
    print(f"TOTAL {passed} / {len(results)}")
    sys.exit(0 if passed == len(results) else 1)


main()
