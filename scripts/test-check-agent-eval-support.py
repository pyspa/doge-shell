#!/usr/bin/env python3
"""Focused behavioral tests for the agent-eval support contract (PER-13).

Boundary under test: `opencode` and generic `command` runners are local-only;
the manual workflow `.github/workflows/agent-eval.yml` intentionally remains
Codex/Claude-only with least-privilege secrets. Stdlib (unittest) only.
"""

import importlib.util
import json
import re
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CHECKER_PATH = ROOT / "scripts" / "check-agent-eval-support.py"
README_PATH = ROOT / "docs" / "ai" / "evals" / "README.md"
WORKFLOW_PATH = ROOT / ".github" / "workflows" / "agent-eval.yml"


def load_checker():
    if not CHECKER_PATH.exists():
        return None
    spec = importlib.util.spec_from_file_location("check_agent_eval_support", CHECKER_PATH)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class SupportBoundaryTest(unittest.TestCase):
    def test_boundary_is_explicit_in_docs(self):
        text = README_PATH.read_text(encoding="utf-8")
        self.assertIn("local-only", text,
                      "README must state the local-only support boundary")
        self.assertIn("opencode", text)
        self.assertIn("command", text)

    def test_checker_exists_and_passes(self):
        self.assertTrue(CHECKER_PATH.exists(),
                        "scripts/check-agent-eval-support.py is missing: "
                        "local runner additions can silently imply workflow support")
        checker = load_checker()
        errors = checker.check()
        self.assertEqual(errors, [], f"support contract drift: {errors}")

    def test_local_opencode_runner_preserved(self):
        checker = load_checker()
        self.assertIsNotNone(checker, "checker missing")
        cli_runners = checker.parse_cli_runners(ROOT / "scripts" / "run-agent-eval.py")
        self.assertIn("opencode", cli_runners,
                      "local --runner opencode must remain supported")

    def test_workflow_stays_codex_claude_only(self):
        checker = load_checker()
        self.assertIsNotNone(checker, "checker missing")
        workflow_runners = checker.parse_workflow_runners(WORKFLOW_PATH)
        self.assertEqual(workflow_runners, {"codex", "claude"})


class DriftRejectionTest(unittest.TestCase):
    def setUp(self):
        self.checker = load_checker()
        if self.checker is None:
            self.fail("checker missing: cannot verify drift rejection")

    def _write(self, directory, name, content):
        path = Path(directory) / name
        path.write_text(content, encoding="utf-8")
        return path

    def test_rejects_undeclared_workflow_runner(self):
        with tempfile.TemporaryDirectory() as tmp:
            workflow = self._write(tmp, "agent-eval.yml", (
                "on:\n  workflow_dispatch:\n    inputs:\n"
                "      runner:\n        type: choice\n"
                "        options: [codex, claude, opencode]\n"
                "jobs:\n  evaluate:\n    steps:\n"
                "      - run: echo ${{ secrets.OPENAI_API_KEY }}\n"
            ))
            errors = self.checker.check(workflow_path=workflow)
            self.assertTrue(errors, "checker must reject a newly undeclared workflow runner")

    def test_rejects_missing_local_opencode(self):
        with tempfile.TemporaryDirectory() as tmp:
            cli = self._write(tmp, "run-agent-eval.py",
                              'parser.add_argument("--runner", choices=("codex", "claude", "command"))\n')
            errors = self.checker.check(cli_path=cli)
            self.assertTrue(errors, "checker must reject local OpenCode disappearing")

    def test_rejects_broadened_secret_set(self):
        with tempfile.TemporaryDirectory() as tmp:
            workflow = self._write(tmp, "agent-eval.yml", (
                "on:\n  workflow_dispatch:\n    inputs:\n"
                "      runner:\n        type: choice\n"
                "        options: [codex, claude]\n"
                "jobs:\n  evaluate:\n    steps:\n"
                "      - run: echo ${{ secrets.OPENAI_API_KEY }} ${{ secrets.EXTRA_PROVIDER_TOKEN }}\n"
            ))
            errors = self.checker.check(workflow_path=workflow)
            self.assertTrue(errors, "checker must reject broadened workflow secrets")

    def test_rejects_schema_runner_drift(self):
        with tempfile.TemporaryDirectory() as tmp:
            schema = {
                "properties": {
                    "normalized": {
                        "properties": {
                            "runner": {"enum": ["codex", "claude", "opencode"]}
                        }
                    }
                }
            }
            schema_path = self._write(tmp, "run-result.schema.json", json.dumps(schema))
            errors = self.checker.check(schema_path=schema_path)
            self.assertTrue(errors, "checker must reject schema runner-enum drift")

    def test_fails_closed_on_unrecognized_workflow_format(self):
        with tempfile.TemporaryDirectory() as tmp:
            workflow = self._write(tmp, "agent-eval.yml", "not a workflow at all\n")
            with self.assertRaises(ValueError):
                self.checker.parse_workflow_runners(workflow)

    def test_block_runner_options_ignores_preceding_case_list(self):
        with tempfile.TemporaryDirectory() as tmp:
            workflow = self._write(tmp, "agent-eval.yml", (
                "name: Agent evaluation\n"
                "on:\n  workflow_dispatch:\n    inputs:\n"
                "      case:\n        type: choice\n        options:\n"
                "          - wait-n-completed-ledger\n"
                "          - pipeline-partial-completion\n"
                "      runner:\n        type: choice\n        options:\n"
                "          - codex\n          - claude\n"
                "jobs:\n  evaluate:\n    steps:\n"
                "      - run: echo ${{ secrets.OPENAI_API_KEY }}\n"
            ))
            self.assertEqual(self.checker.parse_workflow_runners(workflow), {"codex", "claude"})

    def test_block_runner_options_ignores_trailing_case_list(self):
        with tempfile.TemporaryDirectory() as tmp:
            workflow = self._write(tmp, "agent-eval.yml", (
                "name: Agent evaluation\n"
                "on:\n  workflow_dispatch:\n    inputs:\n"
                "      runner:\n        type: choice\n        options:\n"
                "          - codex\n          - claude\n"
                "      case:\n        type: choice\n        options:\n"
                "          - wait-n-completed-ledger\n"
                "          - pipeline-partial-completion\n"
                "jobs:\n  evaluate:\n    steps:\n"
                "      - run: echo ${{ secrets.OPENAI_API_KEY }}\n"
            ))
            self.assertEqual(self.checker.parse_workflow_runners(workflow), {"codex", "claude"})

    def test_absent_runner_stanza_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            workflow = self._write(tmp, "agent-eval.yml", (
                "name: Agent evaluation\n"
                "on:\n  workflow_dispatch:\n    inputs:\n"
                "      case:\n        type: choice\n        options:\n"
                "          - wait-n-completed-ledger\n"
                "jobs:\n  evaluate:\n    steps:\n"
                "      - run: echo ${{ secrets.OPENAI_API_KEY }}\n"
            ))
            with self.assertRaises(ValueError):
                self.checker.parse_workflow_runners(workflow)

    def test_runner_stanza_without_options_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            workflow = self._write(tmp, "agent-eval.yml", (
                "name: Agent evaluation\n"
                "on:\n  workflow_dispatch:\n    inputs:\n"
                "      runner:\n        type: choice\n"
                "      case:\n        type: choice\n        options:\n"
                "          - wait-n-completed-ledger\n"
                "jobs:\n  evaluate:\n    steps:\n"
                "      - run: echo ${{ secrets.OPENAI_API_KEY }}\n"
            ))
            with self.assertRaises(ValueError):
                self.checker.parse_workflow_runners(workflow)

    def test_comment_only_dispatch_reference_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            workflow = self._write(tmp, "agent-eval.yml", (
                "# note: workflow_dispatch configuration lives in the real workflow\n"
                "inputs:\n"
                "  runner:\n"
                "    type: choice\n"
                "    options: [codex, claude]\n"
            ))
            with self.assertRaises(ValueError):
                self.checker.parse_workflow_runners(workflow)

    def test_workflow_call_inputs_ignored(self):
        with tempfile.TemporaryDirectory() as tmp:
            workflow = self._write(tmp, "agent-eval.yml", (
                "name: Agent evaluation\n"
                "on:\n"
                "  workflow_call:\n"
                "    inputs:\n"
                "      runner:\n"
                "        type: choice\n"
                "        options: [codex]\n"
                "  workflow_dispatch:\n"
                "    inputs:\n"
                "      runner:\n"
                "        type: choice\n"
                "        options: [codex, claude]\n"
                "jobs:\n  evaluate:\n    steps:\n"
                "      - run: echo ${{ secrets.OPENAI_API_KEY }}\n"
            ))
            self.assertEqual(self.checker.parse_workflow_runners(workflow), {"codex", "claude"})

    def test_declaration_sets_are_independent(self):
        self.assertEqual(set(self.checker.WORKFLOW_RUNNERS), {"codex", "claude"})
        self.assertEqual(set(self.checker.LOCAL_ONLY_RUNNERS), {"opencode", "command"})
        self.assertEqual(set(self.checker.LOCAL_RUNNERS),
                         set(self.checker.WORKFLOW_RUNNERS) | set(self.checker.LOCAL_ONLY_RUNNERS))

    def test_readme_boundary_statement_is_anchored(self):
        errors = self.checker.check_readme(README_PATH)
        self.assertEqual(errors, [], f"boundary statement weakened: {errors}")

    def test_readme_scattered_terms_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            readme = self._write(tmp, "README.md", (
                "# Agent evaluation\n"
                "\n"
                "The opencode adapter and generic command runner are useful locally.\n"
                "\n"
                "Run evaluations in a trusted local-only environment.\n"
                "\n"
                "CI stays Codex/Claude-only.\n"
            ))
            errors = self.checker.check_readme(readme)
            self.assertTrue(errors, "checker must require the boundary terms in one statement")


if __name__ == "__main__":
    sys.exit(0 if unittest.main(exit=False).result.wasSuccessful() else 1)
