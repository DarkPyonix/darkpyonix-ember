"""Runs the dpx/agents transcript parsers over the shared vectors in testdata/transcripts/.

The vectors' expected.json is the normalised form (testdata/transcripts/SCHEMA.md). These
adapters are display parsers that summarise tool calls instead of keeping them, so the
comparison is a projection of it: the ordered (role, text) of plain user/assistant messages,
the cwd, and the title where the file names one.

    cd proxy && python3 -m unittest
"""
from __future__ import annotations

import json
import shutil
import tempfile
import unittest
from pathlib import Path

from dpx.agents.claude_code import ClaudeCodeAdapter
from dpx.agents.codex import CodexAdapter

VECTORS = Path(__file__).resolve().parents[2] / "testdata" / "transcripts"

# agent dir -> (adapter class, where its files live relative to the adapter root)
AGENTS = {
    "claude-code": (ClaudeCodeAdapter, "-work-project"),
    "codex": (CodexAdapter, "2026/01/02"),
}
CID = "vector-session"


def cases(agent: str) -> list[Path]:
    return sorted(p for p in (VECTORS / agent).iterdir() if (p / "input.jsonl").is_file())


def expected_projection(expected: dict) -> list[tuple[str, str]]:
    return [
        (m["role"], m["text"])
        for m in expected["messages"]
        if m["role"] in ("user", "assistant") and "tool_name" not in m
    ]


def parse(agent: str, case: Path) -> tuple[list[tuple[str, str]], object]:
    """Runs the adapter over the case's input.jsonl, laid out the way the agent stores it."""
    cls, subdir = AGENTS[agent]
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        folder = root / subdir
        folder.mkdir(parents=True)
        shutil.copyfile(case / "input.jsonl", folder / f"{CID}.jsonl")
        adapter = cls(root)
        messages = adapter.transcript(CID, limit=0)
        conv = next(c for c in adapter.conversations() if c.id == CID)
    projection = [(m.role, m.text) for m in messages if m.kind == "text" and m.role in ("user", "assistant")]
    return projection, conv


class TranscriptVectors(unittest.TestCase):
    def test_vectors_exist(self):
        for agent in AGENTS:
            self.assertTrue(cases(agent), f"no vectors for {agent}")

    def check(self, agent: str):
        for case in cases(agent):
            with self.subTest(agent=agent, case=case.name):
                expected = json.loads((case / "expected.json").read_text(encoding="utf-8"))
                got, conv = parse(agent, case)
                self.assertEqual(got, expected_projection(expected))
                if expected["cwd"] is not None:
                    self.assertEqual(conv.folder, expected["cwd"])
                else:   # the adapters fall back to the containing folder's name
                    self.assertEqual(conv.folder, Path(AGENTS[agent][1]).name)
                if expected["title"] is not None:
                    self.assertEqual(conv.title, expected["title"])

    def test_claude_code(self):
        self.check("claude-code")

    def test_codex(self):
        self.check("codex")


if __name__ == "__main__":
    unittest.main()
