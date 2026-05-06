#!/usr/bin/env python3
# sync-safe
"""
Memory Search — Search across all indexed files using FTS5.

Usage:
    python3 memory_search.py "query terms"
    python3 memory_search.py --top 5 "exact phrase"
    python3 memory_search.py --file people "name"
    python3 memory_search.py --context 3 "keyword"
    python3 memory_search.py --compact "keyword"
    python3 memory_search.py --private "sensitive"

Part of the Hex memory system.
"""

import sys
import sqlite3
import argparse
import re
import json
import os
import struct
from pathlib import Path

# --- Optional hybrid search deps ---
try:
    import sqlite_vec
    HAS_VEC = True
except ImportError:
    HAS_VEC = False

try:
    from fastembed import TextEmbedding
    HAS_EMBED = True
except ImportError:
    HAS_EMBED = False

EMBED_MODEL = "BAAI/bge-small-en-v1.5"
EMBED_DIM = 384
_embedder = None

def _get_embedder():
    global _embedder
    if _embedder is None and HAS_EMBED:
        _embedder = TextEmbedding(model_name=EMBED_MODEL)
    return _embedder


def _embed_query(query: str) -> "list[float] | None":
    embedder = _get_embedder()
    if embedder is None:
        return None
    embeddings = list(embedder.embed([query]))
    return embeddings[0].tolist()


def _vec_search(conn: sqlite3.Connection, query_embedding: list, top_n: int = 50) -> list:
    """Search vec_chunks for nearest neighbors. Returns list of (chunk_rowid, distance)."""
    emb_json = json.dumps(query_embedding)
    rows = conn.execute(
        f"SELECT chunk_rowid, distance FROM vec_chunks WHERE embedding MATCH ? ORDER BY distance LIMIT ?",
        (emb_json, top_n),
    ).fetchall()
    return rows


def _rrf_merge(fts_results: list, vec_results: list, top_n: int = 10, k: int = 60) -> list:
    """Merge FTS5 and vector results using Reciprocal Rank Fusion.

    fts_results: list of (source_path, heading, chunk_index, content, score) from FTS5
    vec_results: list of (chunk_rowid, distance) from vec_chunks

    Returns merged list in FTS5 result format, sorted by RRF score.

    KNOWN GAP (2026-05-06 audit): the current implementation builds vec_rowids
    and discards them — final ordering uses FTS5 rank only. Real RRF fusion
    would need a chunks-by-rowid lookup to add vec contributions to the score
    for items not in fts_results. TODO. Until fixed, --hybrid pays the
    embedding + vec-query cost without using the result.
    """
    # Build RRF scores from FTS5 ranks
    rrf_scores = {}  # chunk_key -> score
    fts_by_key = {}  # chunk_key -> full result tuple

    for rank, result in enumerate(fts_results):
        key = (result[0], result[1], result[2])  # (source_path, heading, chunk_index)
        rrf_scores[key] = rrf_scores.get(key, 0) + 1.0 / (k + rank + 1)
        fts_by_key[key] = result

    # We need to look up chunk info for vector results
    # vec_results only has (chunk_rowid, distance) so we need the rowid->key mapping
    # For now, just boost FTS results that also appear in vector results
    vec_rowids = {r[0] for r in vec_results}

    # Add vector rank contribution for FTS results whose rowids appear in vec results
    # (We can't easily map vec rowids back to FTS keys without another query,
    # so we use a simpler approach: boost FTS scores by vec rank when both match)

    # For pure vector results not in FTS, we'd need another DB lookup.
    # For now, RRF is applied only to FTS candidates that also have vector support.
    # This is a practical simplification that avoids extra DB queries.

    # Sort by RRF score
    sorted_results = sorted(fts_by_key.values(), key=lambda r: rrf_scores.get((r[0], r[1], r[2]), 0), reverse=True)
    return sorted_results[:top_n]


def _find_root():
    """Walk up from script location to find the agent root."""
    d = Path(__file__).resolve().parent
    for _ in range(6):
        if (d / "CLAUDE.md").exists():
            return d
        d = d.parent
    return Path(__file__).resolve().parent.parent


AGENT_ROOT = _find_root()
DB_PATH = AGENT_ROOT / ".hex" / "memory.db"

# ── Dual-schema memory paths ─────────────────────────────────────────────────
# Claude Code stores memory at: .claude/projects/<slug>/memory.jsonl
# Codex stores memory at:       .codex/memory/memory.jsonl
# Both are checked when searching JSONL-based memory files.

def _get_memory_jsonl_paths():
    """Return all memory.jsonl paths for the active runtime(s)."""
    paths = []
    home = Path(os.environ.get("HOME", str(Path.home())))
    # Claude Code: per-project memory
    claude_projects = home / ".claude" / "projects"
    if claude_projects.exists():
        paths.extend(claude_projects.rglob("memory.jsonl"))
    # Codex: global memory
    codex_mem = home / ".codex" / "memory" / "memory.jsonl"
    if codex_mem.exists():
        paths.append(codex_mem)
    return paths


def _normalize_memory_entry(entry: dict) -> dict:
    """Normalize memory entries across Claude (text/timestamp) and Codex (content/created_at) schemas."""
    if "content" in entry and "text" not in entry:
        entry["text"] = entry["content"]
    if "created_at" in entry and "timestamp" not in entry:
        entry["timestamp"] = entry["created_at"]
    return entry


def truncate(text: str, max_chars: int = 300) -> str:
    """Truncate text to max_chars, ending at a word boundary."""
    if len(text) <= max_chars:
        return text
    truncated = text[:max_chars].rsplit(" ", 1)[0]
    return truncated + "..."


def highlight_terms(text: str, query: str) -> str:
    """Bold matching terms in output (using ANSI codes)."""
    terms = query.lower().split()
    result = text
    for term in terms:
        pattern = re.compile(re.escape(term), re.IGNORECASE)
        result = pattern.sub(lambda m: f"\033[1;33m{m.group()}\033[0m", result)
    return result


def search(query: str, top_n: int = 10, file_filter: str = None) -> list:
    """Search the FTS5 index."""
    if not DB_PATH.exists():
        print("No index found. Run memory_index.py first.")
        sys.exit(1)

    conn = sqlite3.connect(str(DB_PATH))
    # Sanitize FTS5 special characters
    sanitized = re.sub(r'["\*\(\)\{}\^\-\~]', ' ', query.strip())
    terms = [t for t in sanitized.split() if t]

    # Build query variants: phrase → AND (no OR fallback — too noisy)
    if len(terms) > 1:
        queries_to_try = [
            '"' + ' '.join(terms) + '"',                    # phrase: "local LLM server"
            ' '.join(f'"{t}"' for t in terms),              # AND:   "local" "LLM" "server"
        ]
    else:
        queries_to_try = [f'"{terms[0]}"'] if terms else [query]

    # Check if chunk_meta table exists (backwards compatible with old DBs)
    has_meta = bool(conn.execute(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name='chunk_meta'"
    ).fetchone())

    if has_meta:
        sql = """
            SELECT
                chunks.source_path,
                chunks.heading,
                chunks.chunk_index,
                chunks.content,
                bm25(chunks) * COALESCE(cm.source_weight, 1.0) as score
            FROM chunks
            LEFT JOIN chunk_meta cm ON chunks.rowid = cm.chunk_rowid
            WHERE chunks MATCH ?
        """
    else:
        sql = """
            SELECT
                source_path,
                heading,
                chunk_index,
                content,
                bm25(chunks) as score
            FROM chunks
            WHERE chunks MATCH ?
        """

    filter_clause = ""
    filter_params = []
    if file_filter:
        col_prefix = "chunks." if has_meta else ""
        filter_clause = f" AND {col_prefix}source_path LIKE ?"
        filter_params = [f"%{file_filter}%"]

    rows = []
    last_error = None
    for fts_query in queries_to_try:
        params = [fts_query] + filter_params + [top_n]
        try:
            rows = conn.execute(sql + filter_clause + " ORDER BY score LIMIT ?", params).fetchall()
            if rows:
                break  # Got results, stop loosening
        except sqlite3.OperationalError as e:
            last_error = e
            continue

    if not rows and last_error:
        print(f"Search error: {last_error}", file=sys.stderr)

    conn.close()
    return rows


def parse_args():
    """Parse command-line arguments."""
    parser = argparse.ArgumentParser(description="Search memory files")
    parser.add_argument("query", nargs="+", help="Search query")
    parser.add_argument("--top", type=int, default=10, help="Number of results")
    parser.add_argument("--file", type=str, default=None, help="Filter by file path pattern")
    parser.add_argument("--compact", action="store_true", help="Compact output")
    parser.add_argument("--context", type=int, default=None, help="Show N lines of context around match")
    parser.add_argument("--private", action="store_true", help="Exclude sensitive paths (me/, people/, raw/)")
    parser.add_argument("--hybrid", action="store_true", help="Force hybrid FTS5+vector search")
    parser.add_argument("--verbose", action="store_true", help="Show routing info")
    return parser.parse_args()


def execute_search(args):
    """Run search, apply privacy filter. Returns (query_str, results)."""
    query = " ".join(args.query)

    # Hybrid search: if --hybrid flag or deps available, use FTS5+vector RRF
    use_hybrid = getattr(args, 'hybrid', False) or (HAS_VEC and HAS_EMBED and not args.file)

    fts_results = search(query, top_n=max(args.top, 50) if use_hybrid else args.top, file_filter=args.file)

    if use_hybrid and fts_results:
        try:
            conn = sqlite3.connect(str(DB_PATH))
            if HAS_VEC:
                conn.enable_load_extension(True)
                sqlite_vec.load(conn)
                conn.enable_load_extension(False)
            # Check if vec_chunks exists
            has_vec_table = bool(conn.execute(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='vec_chunks'"
            ).fetchone())
            if has_vec_table:
                query_emb = _embed_query(query)
                if query_emb:
                    vec_results = _vec_search(conn, query_emb, top_n=50)
                    results = _rrf_merge(fts_results, vec_results, top_n=args.top)
                    if args.verbose:
                        # Honest accounting: _rrf_merge currently sorts by FTS rank only
                        # (see comment at the function's vec_rowids handling). Vec results
                        # are computed but not fused into the score. TODO: real RRF.
                        print(f"[hybrid: {len(fts_results)} FTS + {len(vec_results)} vec computed; ordering uses FTS only — vec fusion not implemented in _rrf_merge]", file=sys.stderr)
                else:
                    results = fts_results[:args.top]
            else:
                results = fts_results[:args.top]
            conn.close()
        except Exception as e:
            if args.verbose:
                print(f"[hybrid failed, falling back to FTS5: {e}]", file=sys.stderr)
            results = fts_results[:args.top]
    else:
        results = fts_results

    # Privacy mode: filter out sensitive paths
    if args.private:
        sensitive_prefixes = ("me/", "people/", "raw/")
        results = [r for r in results if not any(r[0].startswith(p) for p in sensitive_prefixes)]
    return query, results


def _print_context_content(content, query, context_lines):
    """Print content with N context lines around matching terms."""
    lines = content.split("\n")
    query_terms = query.lower().split()
    matching_indices = set()
    for idx, line in enumerate(lines):
        if any(term in line.lower() for term in query_terms):
            for j in range(max(0, idx - context_lines), min(len(lines), idx + context_lines + 1)):
                matching_indices.add(j)
    if matching_indices:
        prev_idx = -2
        for idx in sorted(matching_indices):
            if idx > prev_idx + 1:
                print("    ...")
            print(f"    {highlight_terms(lines[idx], query)}")
            prev_idx = idx
    else:
        snippet = truncate(content, 500)
        for line in snippet.split("\n"):
            print(f"    {line}")


def format_results(results, args, query):
    """Print formatted search results."""
    if not results:
        print(f"No results for: {query}")
        return

    print(f"\n{'='*60}")
    print(f" Memory Search: \"{query}\" — {len(results)} results")
    print(f"{'='*60}\n")

    for i, (source_path, heading, chunk_idx, content, score) in enumerate(results):
        if args.compact:
            snippet = truncate(content.replace("\n", " "), 100)
            print(f"  [{i+1}] {source_path} > {heading}  (score: {score:.2f})")
            print(f"      {snippet}")
            print()
        else:
            print(f"--- Result {i+1} ---")
            print(f"  File:    {source_path}")
            print(f"  Section: {heading}")
            print(f"  Score:   {score:.2f}")
            print("  Content:")
            if args.context is not None:
                _print_context_content(content, query, args.context)
            else:
                snippet = truncate(content, 500)
                for line in snippet.split("\n"):
                    print(f"    {line}")
            print()

    if len(results) == args.top:
        print(f"(Showing top {args.top}. Use --top N to see more.)")


def main():
    args = parse_args()
    query, results = execute_search(args)
    format_results(results, args, query)


if __name__ == "__main__":
    main()
