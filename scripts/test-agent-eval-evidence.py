"""Regression tests for the evidence used to grade and compare agent runs."""

import json
import runpy
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from agent_eval.grading import run_validation

SCRIPTS = Path(__file__).resolve().parent


class CargoEvidenceTest(unittest.TestCase):
    def validate(self, log, code=0):
        command = "cargo test --lib"
        case = SimpleNamespace(required_tests=[command], required_checkers=[])
        with tempfile.TemporaryDirectory() as directory, patch(
            "agent_eval.grading.bounded_command", return_value=(code, log, False)
        ):
            root = Path(directory)
            return run_validation(case, root, root, 5)[command]

    def test_all_ignored(self):
        result = self.validate("running 1 test\ntest result: ok. 0 passed; 0 failed; 1 ignored;")
        self.assertNotEqual(result["exit_code"], 0)
        self.assertEqual(result["tests_executed"], 0)

    def test_multiple_suites_count_completed_tests(self):
        log = ("running 3 tests\ntest result: ok. 2 passed; 0 failed; 1 ignored;\n"
               "running 1 test\ntest result: FAILED. 0 passed; 1 failed; 0 ignored;")
        result = self.validate(log, 101)
        self.assertEqual(result["exit_code"], 101)
        self.assertEqual(result["tests_executed"], 3)
        self.assertNotIn("error", result)

    def test_exit_before_a_failing_test_is_reported(self):
        result = self.validate("running 1 test\nprocess exited abnormally", 101)
        self.assertNotEqual(result["exit_code"], 0)
        self.assertIn("error", result)


class FixtureSmokeTest(unittest.TestCase):
    def test_existing_checker_failure_cannot_confirm_a_mutation(self):
        smoke_error = runpy.run_path(str(SCRIPTS / "check-agent-eval-fixtures.py"))["smoke_error"]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "check.py").write_text("raise SystemExit(1)\n", encoding="utf-8")
            case = SimpleNamespace(required_tests=[], required_checkers=["python3 check.py"])
            self.assertIn("clean HEAD: python3 check.py: exit 1", smoke_error(case, root, 5))


class ComparisonTest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)

    def add_run(self, side, case, run, passed, tokens=None):
        path = self.root / side / case / str(run)
        path.mkdir(parents=True)
        result = {"normalized": {"case_id": case, "runner": "codex", "model": "example",
                                 "prompt_tokens": tokens, "files_changed": []},
                  "grade": {"outcome": {"pass": passed}}}
        (path / "result.json").write_text(json.dumps(result), encoding="utf-8")

    def compare(self):
        proc = subprocess.run([sys.executable, str(SCRIPTS / "summarize-agent-evals.py"),
                               "--baseline", str(self.root / "before"), "--candidate",
                               str(self.root / "after"), "--json"], text=True, capture_output=True)
        return proc.returncode, json.loads(proc.stdout)

    def test_disjoint_cases_cannot_be_compared(self):
        self.add_run("before", "completion", 1, True)
        self.add_run("after", "wait", 1, True)
        code, data = self.compare()
        self.assertNotEqual(code, 0)
        self.assertTrue(data["warnings"])

    def test_denominators_and_partial_overlap(self):
        self.add_run("before", "wait", 1, True)
        self.add_run("before", "completion", 1, True, 200)
        self.add_run("after", "wait", 1, True, 0)
        self.add_run("after", "wait", 2, False)
        code, data = self.compare()
        self.assertEqual(code, 0)
        self.assertEqual(data["baseline"]["success_rate"], 1.0)
        self.assertEqual(data["candidate"]["success_rate"], 0.5)
        self.assertEqual(data["candidate"]["prompt_token_samples"], 1)
        self.assertEqual(data["candidate"]["median_prompt_tokens"], 0)
        self.assertIsNone(data["baseline"]["median_prompt_tokens"])
        self.assertTrue(any("completion" in warning for warning in data["warnings"]))


if __name__ == "__main__":
    unittest.main()
