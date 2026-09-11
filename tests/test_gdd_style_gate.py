#!/usr/bin/env python3
import contextlib
import ast
import importlib.util
import io
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest


SOURCE = pathlib.Path(__file__).parents[1] / "system" / "scripts" / "gdd-style-gate.py"
SPEC = importlib.util.spec_from_file_location("gdd_style_gate", SOURCE)
GATE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(GATE)


class GddStyleGateTests(unittest.TestCase):
    def run_main(self, payload, log_path=None):
        old_log = GATE.LOG_PATH
        if log_path is not None:
            GATE.LOG_PATH = str(log_path)
        output = io.StringIO()
        errors = io.StringIO()
        try:
            with contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors):
                code = GATE.main(io.StringIO(json.dumps(payload)))
        finally:
            GATE.LOG_PATH = old_log
        return code, output.getvalue(), errors.getvalue()

    def test_direct_payload_is_advisory_and_does_not_store_reply(self):
        with tempfile.TemporaryDirectory() as directory:
            log = pathlib.Path(directory) / "gate.jsonl"
            payload = {"session_id": "s1", "last_assistant_message": "Use a short answer."}
            code, stdout, stderr = self.run_main(payload, log)
            self.assertEqual(code, 0)
            self.assertNotIn("decision", stdout)
            self.assertNotIn("rewrite", stdout.lower())
            self.assertFalse(log.exists())

    def test_worker_false_positive_phrases_do_not_block(self):
        text = ("Recovery remains incomplete.\n\n"
                "- BOI remains paused.\n"
                "- This establishes exposure, not a confirmed miner.")
        with tempfile.TemporaryDirectory() as directory:
            code, stdout, stderr = self.run_main(
                {"session_id": "s2", "last_assistant_message": text},
                pathlib.Path(directory) / "gate.jsonl")
            self.assertEqual(code, 0)
            self.assertEqual(stdout, "")
            self.assertEqual(stderr, "")

    def test_objective_findings_warn_without_block_or_rewrite(self):
        text = "Result — complete.\n\n**Finding:** stale"
        with tempfile.TemporaryDirectory() as directory:
            log = pathlib.Path(directory) / "gate.jsonl"
            code, stdout, stderr = self.run_main(
                {"session_id": "s3", "last_assistant_message": text}, log)
            self.assertEqual(code, 0)
            self.assertEqual(stdout, "")
            self.assertIn("advisory", stderr)
            row = json.loads(log.read_text().splitlines()[0])
            self.assertEqual(row["kind"], "advisory")
            self.assertNotIn("text", row)
            self.assertNotIn("before", row)

    def test_code_quotes_and_structured_json_are_exempt(self):
        texts = [
            "```\nvalue — quoted code\n```",
            "~~~python\nvalue — quoted code\n~~~",
            "    value — indented code",
            "> Evidence — quoted text",
            '{"message": "value — structured"}',
        ]
        for text in texts:
            self.assertEqual(GATE.mechanical_lint(text), [])

    def test_transcript_fallback_reads_current_turn_only(self):
        with tempfile.TemporaryDirectory() as directory:
            transcript = pathlib.Path(directory) / "transcript.jsonl"
            transcript.write_text(
                json.dumps({"type": "user", "message": {"content": "old"}}) + "\n"
                + json.dumps({"type": "assistant", "message": {"content": [{"type": "text", "text": "old — text"}]}}) + "\n"
                + json.dumps({"type": "user", "message": {"content": "new"}}) + "\n"
                + json.dumps({"type": "assistant", "message": {"content": [{"type": "text", "text": "new text"}]}}) + "\n")
            with tempfile.TemporaryDirectory() as logs:
                code, stdout, stderr = self.run_main(
                    {"session_id": "s4", "transcript_path": str(transcript)},
                    pathlib.Path(logs) / "gate.jsonl")
            self.assertEqual(code, 0)
            self.assertEqual(stderr, "")

    def test_malformed_missing_and_large_input_fail_open(self):
        for raw in ["not json", "{}", "x" * (2 * 1024 * 1024)]:
            old_stdin = GATE.sys.stdin
            output, errors = io.StringIO(), io.StringIO()
            try:
                GATE.sys.stdin = io.StringIO(raw)
                with contextlib.redirect_stdout(output), contextlib.redirect_stderr(errors):
                    code = GATE.main()
            finally:
                GATE.sys.stdin = old_stdin
            self.assertEqual(code, 0)
            self.assertNotIn("decision", output.getvalue())

    def test_log_failure_stays_advisory(self):
        with tempfile.TemporaryDirectory() as directory:
            code, stdout, stderr = self.run_main(
                {"session_id": "s5", "last_assistant_message": "Result — complete."},
                pathlib.Path(directory))
            self.assertEqual(code, 0)
            self.assertEqual(stdout, "")
            self.assertIn("log write failed", stderr)

    def test_source_has_no_network_or_secret_judge(self):
        source = SOURCE.read_text()
        for forbidden in ("urllib", "openrouter", "OPENROUTER_API_KEY", "decision\": \"block\""):
            self.assertNotIn(forbidden, source)
        allowed = {"datetime", "json", "os", "re", "stat", "sys", "pathlib"}
        for node in ast.walk(ast.parse(source)):
            if isinstance(node, ast.Import):
                self.assertTrue({item.name for item in node.names} <= allowed)
            elif isinstance(node, ast.ImportFrom):
                self.assertIn(node.module, allowed)

    def test_real_cli_never_rejects_or_rewrites_worker_and_retry_payloads(self):
        with tempfile.TemporaryDirectory() as directory:
            log = pathlib.Path(directory) / "advisory.jsonl"
            env = dict(os.environ, GDD_GATE_LOG=str(log))
            verdict = {"status": "failed", "reason": "tests failed — preserve this verbatim"}
            payloads = [
                "not-json", "[]", "{}",
                json.dumps({"last_assistant_message": "# Heading — intentionally violates format"}),
                json.dumps({"last_assistant_message": "BOI remains paused."}),
                json.dumps({"last_assistant_message": "[" * 2000 + "]" * 2000}),
                json.dumps({"last_assistant_message": json.dumps(verdict), "cwd": directory}),
                json.dumps({"last_assistant_message": "# Retry — unchanged", "stop_hook_active": True}),
                json.dumps({"transcript_path": str(pathlib.Path(directory) / "absent.jsonl")}),
                "x" * (GATE.MAX_INPUT + 1),
            ]
            for payload in payloads:
                result = subprocess.run(
                    [sys.executable, "-I", "-B", str(SOURCE)], input=payload,
                    capture_output=True, text=True, env=env, timeout=3,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout, "")
            rows = [json.loads(line) for line in log.read_text().splitlines()]
            self.assertEqual(len(rows), 1, "Only the intentional human format violation should log")
            self.assertTrue(rows[0]["advisory"])
            self.assertNotIn("preserve this verbatim", log.read_text())

    def test_retry_skips_findings_and_storage(self):
        with tempfile.TemporaryDirectory() as directory:
            log = pathlib.Path(directory) / "retry.jsonl"
            code, stdout, stderr = self.run_main(
                {"last_assistant_message": "# Wrong — format", "stop_hook_active": True}, log)
            self.assertEqual((code, stdout, stderr), (0, "", ""))
            self.assertFalse(log.exists())

    @unittest.skipUnless(hasattr(os, "mkfifo"), "POSIX file-boundary test")
    def test_fifo_transcript_and_log_return_without_waiting(self):
        with tempfile.TemporaryDirectory() as directory:
            fifo = pathlib.Path(directory) / "pipe"
            os.mkfifo(fifo)
            cases = [
                ({"transcript_path": str(fifo)}, pathlib.Path(directory) / "regular.jsonl"),
                ({"last_assistant_message": "Result — complete."}, fifo),
            ]
            for payload, log in cases:
                result = subprocess.run(
                    [sys.executable, "-I", "-B", str(SOURCE)], input=json.dumps(payload),
                    capture_output=True, text=True,
                    env=dict(os.environ, GDD_GATE_LOG=str(log)), timeout=2,
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout, "")
                self.assertIn("failed", result.stderr)

    def test_transcript_tail_can_start_inside_utf8_character(self):
        with tempfile.TemporaryDirectory() as directory:
            transcript = pathlib.Path(directory) / "unicode.jsonl"
            prefix = ("é" * GATE.MAX_TRANSCRIPT).encode("utf-8")
            last = json.dumps({"type": "assistant", "message": {"content": "Current reply."}}).encode()
            transcript.write_bytes(prefix + b"\n" + last + b"\n")
            self.assertEqual(GATE.transcript_final(transcript), "Current reply.")


if __name__ == "__main__":
    unittest.main(verbosity=2)
