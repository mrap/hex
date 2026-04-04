#!/usr/bin/env python3
"""hex test suite — validates memory save, search, index, and setup.

Usage:
    python3 tests/test_hex.py          # Run all tests
    python3 tests/test_hex.py -v       # Verbose output

Requires: Python 3.8+ with sqlite3 FTS5 support.
Zero dependencies — stdlib only.
"""

import os
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import unittest

# Resolve paths relative to repo root
REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SETUP_SH = os.path.join(REPO_ROOT, "setup.sh")


class HexTestBase(unittest.TestCase):
    """Base class that sets up a fresh hex workspace per test."""

    def setUp(self):
        self.workdir = tempfile.mkdtemp(prefix="hex-test-")
        # Copy hex files into temp workspace
        shutil.copytree(
            os.path.join(REPO_ROOT, ".hex"),
            os.path.join(self.workdir, ".hex"),
        )
        shutil.copy(SETUP_SH, self.workdir)
        # Run setup
        result = subprocess.run(
            ["bash", "setup.sh"],
            cwd=self.workdir,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, f"setup.sh failed: {result.stderr}")

    def tearDown(self):
        shutil.rmtree(self.workdir, ignore_errors=True)

    def run_save(self, content, tags="", source=""):
        cmd = ["python3", ".hex/memory/save.py", content]
        if tags:
            cmd += ["--tags", tags]
        if source:
            cmd += ["--source", source]
        return subprocess.run(cmd, cwd=self.workdir, capture_output=True, text=True)

    def run_search(self, query, extra_args=None):
        cmd = ["python3", ".hex/memory/search.py", query]
        if extra_args:
            cmd += extra_args
        return subprocess.run(cmd, cwd=self.workdir, capture_output=True, text=True)

    def run_index(self, extra_args=None):
        cmd = ["python3", ".hex/memory/index.py"]
        if extra_args:
            cmd += extra_args
        return subprocess.run(cmd, cwd=self.workdir, capture_output=True, text=True)

    def run_stats(self):
        return subprocess.run(
            ["python3", ".hex/stats.py"],
            cwd=self.workdir,
            capture_output=True,
            text=True,
        )


class TestSetup(HexTestBase):
    """Test setup.sh creates correct structure."""

    def test_setup_creates_db(self):
        db_path = os.path.join(self.workdir, ".hex", "memory", "memory.db")
        self.assertTrue(os.path.exists(db_path), "memory.db not created")

    def test_setup_creates_directories(self):
        for subdir in ["memory", "landings", "evolution", "standing-orders"]:
            path = os.path.join(self.workdir, ".hex", subdir)
            self.assertTrue(os.path.isdir(path), f".hex/{subdir} not created")

    def test_setup_creates_landing(self):
        landings = os.path.join(self.workdir, ".hex", "landings")
        md_files = [f for f in os.listdir(landings) if f != "TEMPLATE.md"]
        self.assertGreater(len(md_files), 0, "No today's landing created")

    def test_setup_idempotent(self):
        """Running setup.sh twice should not error or duplicate data."""
        result = subprocess.run(
            ["bash", "setup.sh"],
            cwd=self.workdir,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, "Second setup.sh run failed")

    def test_db_has_fts5(self):
        db_path = os.path.join(self.workdir, ".hex", "memory", "memory.db")
        conn = sqlite3.connect(db_path)
        c = conn.cursor()
        c.execute("SELECT name FROM sqlite_master WHERE type='table'")
        tables = {row[0] for row in c.fetchall()}
        conn.close()
        self.assertIn("memories", tables)
        self.assertIn("memories_fts", tables)


class TestSave(HexTestBase):
    """Test memory saving."""

    def test_save_basic(self):
        result = self.run_save("Test memory content")
        self.assertEqual(result.returncode, 0)
        self.assertIn("Saved memory #1", result.stdout)

    def test_save_with_tags(self):
        result = self.run_save("Tagged content", tags="tag1,tag2")
        self.assertEqual(result.returncode, 0)
        self.assertIn("Saved", result.stdout)

    def test_save_with_source(self):
        result = self.run_save("Sourced content", source="test.py")
        self.assertEqual(result.returncode, 0)
        self.assertIn("Saved", result.stdout)

    def test_save_empty_rejected(self):
        result = self.run_save("")
        self.assertNotEqual(result.returncode, 0, "Empty content should be rejected")
        self.assertIn("empty", result.stderr.lower())

    def test_save_whitespace_only_rejected(self):
        result = self.run_save("   ")
        self.assertNotEqual(result.returncode, 0, "Whitespace-only content should be rejected")

    def test_save_unicode(self):
        result = self.run_save("Unicode test: 日本語 🎯 émojis")
        self.assertEqual(result.returncode, 0)
        self.assertIn("Saved", result.stdout)

    def test_save_special_chars(self):
        """SQL injection should not break anything."""
        result = self.run_save("'; DROP TABLE memories; --")
        self.assertEqual(result.returncode, 0)
        # Verify table still exists
        result2 = self.run_search("DROP")
        self.assertEqual(result2.returncode, 0)

    def test_save_long_content(self):
        result = self.run_save("x" * 5000)
        self.assertEqual(result.returncode, 0)
        self.assertIn("...", result.stdout)  # Should truncate display

    def test_save_multiline(self):
        result = self.run_save("Line 1\nLine 2\nLine 3")
        self.assertEqual(result.returncode, 0)

    def test_save_increments_count(self):
        self.run_save("First memory")
        result = self.run_save("Second memory")
        self.assertIn("2 memories total", result.stdout)

    def test_save_no_db(self):
        """Save without setup should give helpful error."""
        tmpdir = tempfile.mkdtemp(prefix="hex-nodb-")
        os.makedirs(os.path.join(tmpdir, ".hex", "memory"))
        shutil.copy(
            os.path.join(REPO_ROOT, ".hex", "memory", "save.py"),
            os.path.join(tmpdir, ".hex", "memory", "save.py"),
        )
        result = subprocess.run(
            ["python3", ".hex/memory/save.py", "test"],
            cwd=tmpdir,
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("setup.sh", result.stderr)
        shutil.rmtree(tmpdir, ignore_errors=True)


class TestSearch(HexTestBase):
    """Test memory search."""

    def test_search_exact_match(self):
        self.run_save("Express uses JWT auth tokens", tags="auth")
        result = self.run_search("JWT")
        self.assertEqual(result.returncode, 0)
        self.assertIn("JWT", result.stdout)

    def test_search_no_results(self):
        result = self.run_search("nonexistent_query_xyz123")
        self.assertEqual(result.returncode, 0)
        self.assertIn("No memories found", result.stdout)

    def test_search_prefix_match(self):
        """'auth' should find 'authentication' via prefix expansion."""
        self.run_save("Authentication middleware handles login flow")
        result = self.run_search("auth")
        self.assertEqual(result.returncode, 0)
        self.assertIn("Authentication", result.stdout)

    def test_search_compact_mode(self):
        self.run_save("Compact test content", tags="test")
        result = self.run_search("Compact", extra_args=["--compact"])
        self.assertEqual(result.returncode, 0)
        self.assertIn("[1]", result.stdout)

    def test_search_top_limit(self):
        for i in range(5):
            self.run_save(f"Memory number {i} about testing")
        result = self.run_search("testing", extra_args=["--top", "2"])
        self.assertEqual(result.returncode, 0)
        self.assertIn("2 results", result.stdout)

    def test_search_by_tag(self):
        self.run_save("Content here", tags="special-tag")
        result = self.run_search("special-tag")
        self.assertEqual(result.returncode, 0)
        self.assertIn("Content here", result.stdout)

    def test_search_empty_query_rejected(self):
        result = self.run_search("")
        self.assertNotEqual(result.returncode, 0)

    def test_search_fts5_syntax(self):
        """FTS5 operators should work."""
        self.run_save("Express framework with JWT authentication")
        self.run_save("React framework with OAuth tokens")
        result = self.run_search('Express AND JWT')
        self.assertEqual(result.returncode, 0)
        self.assertIn("Express", result.stdout)

    def test_search_malformed_fts5_no_crash(self):
        """Broken FTS5 syntax should not crash."""
        self.run_save("Some content")
        result = self.run_search('"unclosed quote')
        self.assertEqual(result.returncode, 0)  # Should not crash

    def test_search_like_fallback(self):
        """LIKE fallback should find substring matches FTS5 misses."""
        self.run_save("The httpOnly cookie flag prevents XSS")
        result = self.run_search("httpOnly")
        self.assertEqual(result.returncode, 0)
        self.assertIn("httpOnly", result.stdout)

    def test_search_logs_queries(self):
        """Search queries should be logged for stats."""
        self.run_save("test content")
        self.run_search("test")
        db_path = os.path.join(self.workdir, ".hex", "memory", "memory.db")
        conn = sqlite3.connect(db_path)
        c = conn.cursor()
        c.execute("SELECT query FROM search_log")
        queries = [row[0] for row in c.fetchall()]
        conn.close()
        self.assertIn("test", queries)


class TestIndex(HexTestBase):
    """Test markdown indexing."""

    def test_index_no_files(self):
        """Indexing empty dir should report 0."""
        tmpdir = tempfile.mkdtemp(prefix="hex-empty-")
        result = self.run_index(extra_args=["--dir", tmpdir])
        self.assertEqual(result.returncode, 0)
        self.assertIn("Indexed 0 chunks", result.stdout)
        shutil.rmtree(tmpdir, ignore_errors=True)

    def test_index_markdown_file(self):
        md_path = os.path.join(self.workdir, "test-doc.md")
        with open(md_path, "w") as f:
            f.write("# Architecture\n\nWe use microservices.\n\n## Database\n\nPostgreSQL for persistence.\n")
        result = self.run_index(extra_args=["--dir", self.workdir])
        self.assertEqual(result.returncode, 0)
        self.assertIn("Indexed", result.stdout)
        # Verify indexed content is searchable
        search_result = self.run_search("microservices")
        self.assertIn("microservices", search_result.stdout)

    def test_index_incremental(self):
        """Second index should skip unchanged files."""
        md_path = os.path.join(self.workdir, "test.md")
        with open(md_path, "w") as f:
            f.write("# Test\n\nContent here.\n")
        self.run_index(extra_args=["--dir", self.workdir])
        result = self.run_index(extra_args=["--dir", self.workdir])
        self.assertIn("unchanged", result.stdout)

    def test_index_full_rebuild(self):
        md_path = os.path.join(self.workdir, "test.md")
        with open(md_path, "w") as f:
            f.write("# Test\n\nRebuild content.\n")
        self.run_index(extra_args=["--dir", self.workdir])
        result = self.run_index(extra_args=["--full", "--dir", self.workdir])
        self.assertEqual(result.returncode, 0)
        self.assertIn("Full rebuild", result.stdout)

    def test_index_skips_git(self):
        """Should not index .git directory."""
        git_dir = os.path.join(self.workdir, ".git")
        os.makedirs(git_dir, exist_ok=True)
        with open(os.path.join(git_dir, "test.md"), "w") as f:
            f.write("# Git internal\n\nShould not be indexed.\n")
        result = self.run_index(extra_args=["--dir", self.workdir])
        search_result = self.run_search("Git internal")
        self.assertIn("No memories found", search_result.stdout)


class TestStats(HexTestBase):
    """Test stats command."""

    def test_stats_empty(self):
        result = self.run_stats()
        self.assertEqual(result.returncode, 0)
        self.assertIn("hex stats", result.stdout)
        self.assertIn("Memories:", result.stdout)

    def test_stats_with_data(self):
        self.run_save("Memory for stats test")
        self.run_search("stats")
        result = self.run_stats()
        self.assertEqual(result.returncode, 0)
        self.assertIn("1", result.stdout)  # At least 1 memory


class TestEndToEnd(HexTestBase):
    """Full workflow tests matching README examples."""

    def test_readme_workflow(self):
        """The exact workflow shown in the README should work."""
        # Save
        result = self.run_save(
            "Project uses Express with JWT auth, refresh tokens in httpOnly cookies",
            tags="auth,architecture",
            source="initial-setup",
        )
        self.assertEqual(result.returncode, 0)
        self.assertIn("Saved memory #1", result.stdout)

        # Search — using 'auth' (the actual token that exists)
        result = self.run_search("auth")
        self.assertEqual(result.returncode, 0)
        self.assertIn("Express", result.stdout)
        self.assertIn("JWT", result.stdout)

    def test_multi_memory_workflow(self):
        """Save multiple memories and search across them."""
        self.run_save("Backend: Express + TypeScript", tags="stack")
        self.run_save("Frontend: React 18 with Next.js", tags="stack")
        self.run_save("Database: PostgreSQL with Prisma ORM", tags="stack,database")
        self.run_save("Auth: JWT with refresh tokens in httpOnly cookies", tags="auth")

        # Search should find relevant memories
        result = self.run_search("database")
        self.assertIn("PostgreSQL", result.stdout)

        result = self.run_search("stack", extra_args=["--compact"])
        self.assertEqual(result.returncode, 0)


if __name__ == "__main__":
    unittest.main()
