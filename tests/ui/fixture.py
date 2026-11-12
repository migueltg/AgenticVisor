"""A fake home folder for the interface tests, so they never touch real data.

Two repositories ("alpha" and "beta") with Claude Code sessions, a session folder,
session data and history entries, as Claude Code writes them.
"""
import json
import os
import re
import shutil

IDS = {
    "a1": "11111111-1111-4111-8111-111111111111",
    "a2": "22222222-2222-4222-8222-222222222222",
    "b1": "33333333-3333-4333-8333-333333333333",
}


def encode(path):
    """Claude Code's folder name for a project path."""
    return re.sub(r"[^A-Za-z0-9]", "-", path)


class Fixture:
    def __init__(self, root):
        self.root = root
        self.home = os.path.join(root, "home")
        self.data = os.path.join(root, "data")
        self.alpha = os.path.join(self.home, "work", "alpha")
        self.beta = os.path.join(self.home, "work", "beta")

    def folder(self, project):
        return os.path.join(self.home, ".claude", "projects", encode(project))

    def transcript(self, project, key):
        return os.path.join(self.folder(project), IDS[key] + ".jsonl")

    def build(self):
        shutil.rmtree(self.root, ignore_errors=True)
        for repo in (self.alpha, self.beta):
            os.makedirs(repo)
            with open(os.path.join(repo, "README.md"), "w") as f:
                f.write("the real repository")
        sessions = [("a1", self.alpha, "Fix the alpha bug"), ("a2", self.alpha, "Add alpha tests"), ("b1", self.beta, "Review beta")]
        history = []
        for key, project, prompt in sessions:
            sid = IDS[key]
            os.makedirs(os.path.join(self.folder(project), sid, "subagents"), exist_ok=True)
            with open(os.path.join(self.folder(project), sid, "subagents", "agent.jsonl"), "w") as f:
                f.write("{}\n")
            record = {"type": "user", "cwd": project, "sessionId": sid, "message": {"role": "user", "content": prompt}}
            with open(self.transcript(project, key), "w") as f:
                f.write(json.dumps(record) + "\n")
            os.makedirs(os.path.join(self.home, ".claude", "session-env", sid))
            os.makedirs(os.path.join(self.home, ".claude", "file-history", sid))
            with open(os.path.join(self.home, ".claude", "file-history", sid, "v1"), "w") as f:
                f.write("x")
            history.append({"display": prompt, "pastedContents": {}, "timestamp": 1, "project": project, "sessionId": sid})
        with open(os.path.join(self.home, ".claude", "history.jsonl"), "w") as f:
            f.write("".join(json.dumps(h) + "\n" for h in history))
        os.makedirs(self.data)
        return self

    def history_text(self):
        with open(os.path.join(self.home, ".claude", "history.jsonl")) as f:
            return f.read()
