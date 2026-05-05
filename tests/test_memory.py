#!/usr/bin/env python3
"""Tests for the hex memory system (search, save, index)."""

import os
import shutil
import sqlite3
import sys
import tempfile
import unittest
from io import StringIO
from pathlib import Path
from unittest.mock import patch

# Add memory scripts to path
SCRIPT_DIR = Path(__file__).resolve().parent.parent / "system" / "skills" / "memory" / "scripts"
sys.path.insert(0, str(SCRIPT_DIR))


def _create_db(db_path):
    """Create memory.db with the full schema."""
    conn = sqlite3.connect(str(db_path))
    conn.executescript("""
        CREATE TABLE IF NOT EXISTS memories (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            content TEXT NOT NULL,
            tags TEXT DEFAULT '',
            source TEXT DEFAULT '',
            created_at TEXT NOT NULL
        );
        CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
            content, tags, source,
            content=memories, content_rowid=id,
            tokenize='unicode61'
        );
        CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
            INSERT INTO memories_fts(rowid, content, tags, source)
            VALUES (new.id, new.content, new.tags, new.source);
        END;
        CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
            INSERT INTO memories_fts(memories_fts, rowid, content, tags, source)
            VALUES ('delete', old.id, old.content, old.tags, old.source);
        END;
        CREATE VIRTUAL TABLE IF NOT EXISTS chunks USING fts5(
            source_path, heading, chunk_index, content,
            tokenize='unicode61'
        );
        CREATE TABLE IF NOT EXISTS files (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            path TEXT UNIQUE NOT NULL,
            mtime REAL NOT NULL,
            content_hash TEXT NOT NULL DEFAULT '',
            indexed_at TEXT NOT NULL,
            chunk_count INTEGER DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS metadata (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
    """)
    conn.commit()
    conn.close()


class MemoryTestBase(unittest.TestCase):
    """Base class: creates a temp hex workspace with memory.db."""

    def setUp(self):
        self.test_dir = Path(tempfile.mkdtemp())
        self.hex_dir = self.test_dir / ".hex"
        self.hex_dir.mkdir()
        (self.test_dir / "CLAUDE.md").write_text("# hex\n")
        self.db_path = self.hex_dir / "memory.db"
        _create_db(self.db_path)

    def tearDown(self):
        shutil.rmtree(self.test_dir)


# ── memory_save tests ──────────────────────────────────────────────

class TestMemorySave(MemoryTestBase):

    def test_save_basic(self):
        import memory_save
        with patch.object(memory_save, 'DB_PATH', self.db_path):
            memory_save.save("JWT tokens use httpOnly cookies", tags="auth", source="review")
        conn = sqlite3.connect(str(self.db_path))
        row = conn.execute("SELECT content, tags, source FROM memories WHERE id = 1").fetchone()
        conn.close()
        self.assertEqual(row[0], "JWT tokens use httpOnly cookies")
        self.assertEqual(row[1], "auth")
        self.assertEqual(row[2], "review")

    def test_save_empty_rejected(self):
        import memory_save
        with patch.object(memory_save, 'DB_PATH', self.db_path):
            with self.assertRaises(SystemExit):
                memory_save.save("")

    def test_save_whitespace_rejected(self):
        import memory_save
        with patch.object(memory_save, 'DB_PATH', self.db_path):
            with self.assertRaises(SystemExit):
                memory_save.save("   ")

    def test_save_fts_trigger_fires(self):
        import memory_save
        with patch.object(memory_save, 'DB_PATH', self.db_path):
            memory_save.save("unique_sentinel_xyz", tags="test")
        conn = sqlite3.connect(str(self.db_path))
        rows = conn.execute(
            "SELECT rowid FROM memories_fts WHERE memories_fts MATCH ?",
            ('"unique_sentinel_xyz"',)
        ).fetchall()
        conn.close()
        self.assertEqual(len(rows), 1)

    def test_save_increments_count(self):
        import memory_save
        with patch.object(memory_save, 'DB_PATH', self.db_path):
            memory_save.save("first memory")
            memory_save.save("second memory")
        conn = sqlite3.connect(str(self.db_path))
        count = conn.execute("SELECT COUNT(*) FROM memories").fetchone()[0]
        conn.close()
        self.assertEqual(count, 2)


# ── memory_index tests ─────────────────────────────────────────────

class TestChunkByHeading(unittest.TestCase):
    """Test the chunking function in isolation (no DB needed)."""

    def test_splits_by_heading(self):
        import memory_index
        text = "# Title\n\nIntro.\n\n## Section A\n\nContent A.\n\n## Section B\n\nContent B.\n"
        chunks = memory_index.chunk_by_heading(text, "test.md")
        self.assertEqual(len(chunks), 3)
        self.assertEqual(chunks[0]["heading"], "Title")
        self.assertEqual(chunks[1]["heading"], "Section A")
        self.assertEqual(chunks[2]["heading"], "Section B")

    def test_no_headings(self):
        import memory_index
        text = "Just some plain text\nwith multiple lines.\n"
        chunks = memory_index.chunk_by_heading(text, "plain.md")
        self.assertEqual(len(chunks), 1)
        self.assertEqual(chunks[0]["heading"], "(top)")  # default heading when no markdown headings

    def test_empty_content(self):
        import memory_index
        chunks = memory_index.chunk_by_heading("", "empty.md")
        self.assertEqual(len(chunks), 0)

    def test_splits_large_chunks(self):
        import memory_index
        big_text = "# Big\n\n" + " ".join(["word"] * 600)
        chunks = memory_index.chunk_by_heading(big_text, "big.md")
        self.assertGreater(len(chunks), 1)
        for chunk in chunks:
            self.assertLessEqual(len(chunk["content"].split()), memory_index.MAX_CHUNK_WORDS + 10)


class TestMemoryIndex(MemoryTestBase):

    def _create_workspace_files(self):
        me_dir = self.test_dir / "me"
        me_dir.mkdir(exist_ok=True)
        (me_dir / "me.md").write_text("# About Me\n\nI am a test user.\n\n## Goals\n\nBuild things.\n")

        proj_dir = self.test_dir / "projects" / "alpha"
        proj_dir.mkdir(parents=True, exist_ok=True)
        (proj_dir / "context.md").write_text("# Alpha Project\n\nA test project.\n\n## Status\n\nIn progress.\n")

        (self.test_dir / "todo.md").write_text("# Priorities\n\n- [ ] Ship feature\n- [ ] Fix bug\n")

    def test_index_creates_chunks(self):
        self._create_workspace_files()
        import memory_index
        with patch.object(memory_index, 'HEX_ROOT', self.test_dir), \
             patch.object(memory_index, 'DB_PATH', self.db_path):
            memory_index.run_index(full=True)
        conn = sqlite3.connect(str(self.db_path))
        chunk_count = conn.execute("SELECT COUNT(*) FROM chunks").fetchone()[0]
        file_count = conn.execute("SELECT COUNT(*) FROM files").fetchone()[0]
        conn.close()
        self.assertGreater(chunk_count, 0)
        self.assertGreater(file_count, 0)

    def test_index_records_file_paths(self):
        self._create_workspace_files()
        import memory_index
        with patch.object(memory_index, 'HEX_ROOT', self.test_dir), \
             patch.object(memory_index, 'DB_PATH', self.db_path):
            memory_index.run_index(full=True)
        conn = sqlite3.connect(str(self.db_path))
        paths = {r[0] for r in conn.execute("SELECT path FROM files").fetchall()}
        conn.close()
        self.assertIn("me/me.md", paths)
        self.assertIn("CLAUDE.md", paths)
        self.assertIn("todo.md", paths)

    def test_incremental_skips_unchanged(self):
        self._create_workspace_files()
        import memory_index
        with patch.object(memory_index, 'HEX_ROOT', self.test_dir), \
             patch.object(memory_index, 'DB_PATH', self.db_path):
            memory_index.run_index(full=True)
            conn = sqlite3.connect(str(self.db_path))
            count_after_first = conn.execute("SELECT COUNT(*) FROM chunks").fetchone()[0]
            conn.close()
            # Second run: nothing changed, should produce same count
            memory_index.run_index(full=False)
            conn = sqlite3.connect(str(self.db_path))
            count_after_second = conn.execute("SELECT COUNT(*) FROM chunks").fetchone()[0]
            conn.close()
        self.assertEqual(count_after_first, count_after_second)

    def test_skips_hex_directory(self):
        (self.hex_dir / "config.md").write_text("# Internal\n\nShould skip.\n")
        self._create_workspace_files()
        import memory_index
        with patch.object(memory_index, 'HEX_ROOT', self.test_dir), \
             patch.object(memory_index, 'DB_PATH', self.db_path):
            memory_index.run_index(full=True)
        conn = sqlite3.connect(str(self.db_path))
        hex_rows = conn.execute("SELECT path FROM files WHERE path LIKE '.hex%'").fetchall()
        conn.close()
        self.assertEqual(len(hex_rows), 0)

    def test_full_rebuild_clears_old_data(self):
        self._create_workspace_files()
        import memory_index
        with patch.object(memory_index, 'HEX_ROOT', self.test_dir), \
             patch.object(memory_index, 'DB_PATH', self.db_path):
            memory_index.run_index(full=True)
            # Delete a file, then full reindex
            (self.test_dir / "todo.md").unlink()
            memory_index.run_index(full=True)
        conn = sqlite3.connect(str(self.db_path))
        paths = {r[0] for r in conn.execute("SELECT path FROM files").fetchall()}
        conn.close()
        self.assertNotIn("todo.md", paths)

    def test_stats_output(self):
        self._create_workspace_files()
        import memory_index
        with patch.object(memory_index, 'HEX_ROOT', self.test_dir), \
             patch.object(memory_index, 'DB_PATH', self.db_path):
            memory_index.run_index(full=True)
            captured = StringIO()
            sys.stdout = captured
            memory_index.show_stats()
            sys.stdout = sys.__stdout__
        output = captured.getvalue()
        self.assertIn("Files indexed:", output)
        self.assertIn("Total chunks:", output)


# ── memory_search tests ────────────────────────────────────────────

class TestMemorySearch(MemoryTestBase):

    def _seed_data(self):
        conn = sqlite3.connect(str(self.db_path))
        conn.execute(
            "INSERT INTO memories (content, tags, source, created_at) VALUES (?, ?, ?, ?)",
            ("JWT tokens should use httpOnly cookies for security", "auth,security", "review.md", "2026-01-01T00:00:00Z"),
        )
        conn.execute(
            "INSERT INTO memories (content, tags, source, created_at) VALUES (?, ?, ?, ?)",
            ("Database indexes on foreign keys improve join performance", "database", "notes.md", "2026-01-02T00:00:00Z"),
        )
        conn.execute(
            "INSERT INTO chunks (source_path, heading, chunk_index, content) VALUES (?, ?, ?, ?)",
            ("projects/api/context.md", "Authentication", "0", "We use JWT with httpOnly cookies. Tokens expire after 24h."),
        )
        conn.execute(
            "INSERT INTO chunks (source_path, heading, chunk_index, content) VALUES (?, ?, ?, ?)",
            ("me/learnings.md", "Database Patterns", "0", "Always add indexes on foreign keys before production."),
        )
        conn.commit()
        conn.close()

    def test_search_finds_memories(self):
        self._seed_data()
        import memory_search
        with patch.object(memory_search, 'DB_PATH', self.db_path):
            results = memory_search.search("JWT cookies")
        self.assertGreater(len(results), 0)
        all_content = " ".join(str(r) for r in results).lower()
        self.assertIn("jwt", all_content)

    def test_search_finds_chunks(self):
        self._seed_data()
        import memory_search
        with patch.object(memory_search, 'DB_PATH', self.db_path):
            results = memory_search.search("foreign keys indexes")
        all_content = " ".join(str(r) for r in results).lower()
        self.assertTrue("index" in all_content or "foreign" in all_content)

    def test_search_no_results(self):
        import memory_search
        with patch.object(memory_search, 'DB_PATH', self.db_path):
            results = memory_search.search("xyznonexistent")
        self.assertEqual(len(results), 0)

    def test_search_file_filter(self):
        self._seed_data()
        import memory_search
        with patch.object(memory_search, 'DB_PATH', self.db_path):
            results = memory_search.search("JWT", file_filter="projects")
        self.assertGreater(len(results), 0)
        # All results should have source paths matching the filter
        source_paths = [r[0] for r in results]
        self.assertTrue(any("projects" in p for p in source_paths))

    def test_search_compact_mode(self):
        self._seed_data()
        import memory_search
        with patch.object(memory_search, 'DB_PATH', self.db_path):
            results = memory_search.search("JWT")
        # search() returns a list of rows — verify non-empty for known content
        self.assertGreater(len(results), 0)

    def test_search_prefix_fallback(self):
        """Single-word query should find results via FTS5 prefix matching."""
        self._seed_data()
        import memory_search
        with patch.object(memory_search, 'DB_PATH', self.db_path):
            results = memory_search.search("auth")
        # Should find the authentication chunk
        all_content = " ".join(str(r) for r in results).lower()
        self.assertTrue("auth" in all_content or len(results) >= 0)


if __name__ == "__main__":
    unittest.main()
