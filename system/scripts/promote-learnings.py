#!/usr/bin/env python3
"""promote-learnings.py — Detect recurring patterns in learnings and propose standing orders.

Scans me/learnings.md for dated entries, groups similar ones by category and keywords.
When a pattern has 3+ dated occurrences, generates a candidate standing order and writes
it to evolution/suggestions.md. Sends Telegram notification with Approve/Dismiss buttons.

Part of the Hex system.

Usage:
    python3 promote-learnings.py           # Run analysis
    python3 promote-learnings.py --test    # Self-test with mock data
"""

import hashlib
import json
import logging
import os
import re
import urllib.request
from collections import defaultdict
from datetime import datetime
from pathlib import Path
from typing import Any, Optional

# ---------------------------------------------------------------------------
# Logging
# ---------------------------------------------------------------------------
logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [promote] %(levelname)s %(message)s",
    datefmt="%H:%M:%S",
)
log = logging.getLogger("promote")

# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------
SCRIPT_DIR = Path(__file__).resolve().parent
if SCRIPT_DIR.name == "scripts" and SCRIPT_DIR.parent.name == ".claude":
    AGENT_DIR = SCRIPT_DIR.parents[1]
else:
    AGENT_DIR = Path(os.environ.get("AGENT_DIR", SCRIPT_DIR.parent))

LEARNINGS_FILE = AGENT_DIR / "me" / "learnings.md"
REFLECTIONS_DIR = AGENT_DIR / "raw" / "reflections"
SUGGESTIONS_FILE = AGENT_DIR / "evolution" / "suggestions.md"
CLAUDE_MD = AGENT_DIR / "CLAUDE.md"
PENDING_FILE = AGENT_DIR / "evolution" / ".pending-promotions.json"
CONFIG_PATH = Path.home() / ".config" / "hex" / "telegram.yaml"

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------
MIN_CLUSTER_SIZE = 3
# 0.15 Jaccard: intentionally low to catch near-duplicates with different wording
# (e.g. "prefers concise responses" vs "wants short replies"). Higher thresholds
# (0.3+) miss paraphrased patterns after stemming. Monitor false-positive rate —
# if unrelated entries cluster together, raise toward 0.25.
SIMILARITY_THRESHOLD = 0.15

STOP_WORDS = {
    "a", "an", "the", "is", "are", "was", "were", "be", "been", "being",
    "have", "has", "had", "do", "does", "did", "will", "would", "could",
    "should", "may", "might", "shall", "can", "need", "must", "ought",
    "me", "my", "we", "our", "you", "your", "he", "she", "it",
    "they", "them", "their", "this", "that", "these", "those", "and",
    "but", "nor", "not", "so", "if", "then", "than",
    "too", "very", "just", "about", "above", "after", "again", "all",
    "also", "any", "as", "at", "because", "before", "between", "both",
    "by", "each", "for", "from", "get", "got", "how", "in", "into",
    "its", "like", "more", "most", "of", "off", "on", "once", "only",
    "other", "out", "over", "own", "same", "some", "such", "to", "up",
    "us", "use", "used", "using", "what", "when", "where", "which",
    "while", "who", "whom", "why", "with", "down", "here", "there",
    "through", "during", "under", "until", "even", "still", "already",
    "much", "many", "well", "way", "don", "doesn", "didn", "won",
    "him", "his", "her", "hers", "mine", "ours", "yours",
    "theirs", "agent", "always", "never", "every",
    "often", "sometimes", "wants", "want", "make", "makes", "made",
    "thing", "things", "something", "nothing", "everything",
}


# ---------------------------------------------------------------------------
# Text processing
# ---------------------------------------------------------------------------
def stem(word: str) -> str:
    """Very basic English word stemmer for keyword matching."""
    if len(word) <= 3:
        return word
    for suffix, min_len in [
        ("ingly", 7), ("edly", 6), ("ness", 6), ("ment", 6),
        ("tion", 6), ("sion", 6), ("ably", 6), ("ibly", 6),
        ("ally", 6), ("ful", 5), ("ly", 4), ("ies", 0),
        ("ing", 5), ("est", 5), ("er", 4), ("ed", 4), ("es", 4),
    ]:
        if word.endswith(suffix) and len(word) > min_len:
            if suffix == "ies":
                return word[:-3] + "y"
            return word[: -len(suffix)]
    if word.endswith("s") and not word.endswith("ss") and len(word) > 3:
        return word[:-1]
    return word


def tokenize(text: str) -> set[str]:
    """Extract stemmed, significant words from text."""
    # Remove quoted strings (examples, not patterns)
    text = re.sub(r'"[^"]*"', "", text)
    text = re.sub(r"'[^']*'", "", text)
    # Remove date and imported annotations
    text = re.sub(r"\(\d{4}-\d{2}-\d{2}\)", "", text)
    text = re.sub(r"\(imported\)", "", text)
    words = re.findall(r"[a-z]+", text.lower())
    return {stem(w) for w in words if len(w) > 3 and w not in STOP_WORDS}


# ---------------------------------------------------------------------------
# Data structures
# ---------------------------------------------------------------------------
class Entry:
    """A single learning entry with metadata."""

    __slots__ = ("text", "category", "date", "dates", "tokens")

    def __init__(self, text: str, category: str, date: Optional[str], dates: Optional[list[str]] = None):
        self.text = text
        self.category = category
        self.date = date
        self.dates = dates or ([date] if date else [])
        self.tokens = tokenize(text)


# ---------------------------------------------------------------------------
# Parsing
# ---------------------------------------------------------------------------
def parse_learnings(filepath: Path) -> list[Entry]:
    """Parse learnings.md into structured entries."""
    entries: list[Entry] = []
    current_category = ""

    if not filepath.exists():
        return entries

    for line in filepath.read_text().splitlines():
        stripped = line.strip()
        if stripped.startswith("## "):
            current_category = stripped[3:].strip()
        elif stripped.startswith("- ") and current_category:
            text = stripped[2:].strip()
            # Match single date: (2026-03-07)
            single_match = re.search(r"\((\d{4}-\d{2}-\d{2})\)\s*$", text)
            # Match multiple dates: (2026-03-01, 2026-03-07, 2026-03-08)
            multi_match = re.search(r"\((\d{4}-\d{2}-\d{2}(?:,\s*\d{4}-\d{2}-\d{2})+)\)\s*$", text)
            dates: list[str] = []
            if multi_match:
                dates = re.findall(r"\d{4}-\d{2}-\d{2}", multi_match.group(1))
            elif single_match:
                dates = [single_match.group(1)]
            date = dates[0] if dates else None
            entries.append(Entry(text, current_category, date, dates))

    return entries


def parse_reflections(dirpath: Path) -> list[Entry]:
    """Parse raw/reflections/*.md into entries for clustering."""
    entries: list[Entry] = []
    if not dirpath.is_dir():
        return entries

    for md_file in sorted(dirpath.glob("*.md")):
        # Extract date from filename (YYYY-MM-DD.md)
        date_match = re.match(r"(\d{4}-\d{2}-\d{2})\.md$", md_file.name)
        date = date_match.group(1) if date_match else None

        current_category = "Reflection"
        for line in md_file.read_text().splitlines():
            stripped = line.strip()
            if stripped.startswith("## "):
                current_category = stripped[3:].strip()
            elif stripped.startswith("- ") and len(stripped) > 10:
                text = stripped[2:].strip()
                # Skip meta lines (auto-generated headers, empty observations)
                if text.startswith("_") or text.startswith("Auto-generated"):
                    continue
                entries.append(Entry(text, current_category, date))

    return entries


# ---------------------------------------------------------------------------
# Similarity & clustering
# ---------------------------------------------------------------------------
def jaccard(a: set, b: set) -> float:
    """Jaccard similarity between two sets."""
    if not a or not b:
        return 0.0
    return len(a & b) / len(a | b)


def find_clusters(entries: list[Entry]) -> list[list[Entry]]:
    """Find clusters of similar dated entries within the same category.

    Uses union-find for clustering: O(n²) pairwise Jaccard comparison within each
    category, then union-find to merge transitively similar entries. This is simple
    and sufficient for <500 entries. DBSCAN or hierarchical clustering would add
    dependency complexity (scipy/sklearn) without meaningful benefit at this scale.
    Revisit if entry count exceeds ~1000 or if category-blind clustering is needed.
    """
    by_category: dict[str, list[Entry]] = defaultdict(list)
    for e in entries:
        if e.date:
            by_category[e.category].append(e)

    all_clusters: list[list[Entry]] = []

    for cat_entries in by_category.values():
        if len(cat_entries) < MIN_CLUSTER_SIZE:
            continue

        n = len(cat_entries)
        parent = list(range(n))

        def find(x: int) -> int:
            while parent[x] != x:
                parent[x] = parent[parent[x]]
                x = parent[x]
            return x

        def union(x: int, y: int) -> None:
            px, py = find(x), find(y)
            if px != py:
                parent[px] = py

        for i in range(n):
            for j in range(i + 1, n):
                if jaccard(cat_entries[i].tokens, cat_entries[j].tokens) >= SIMILARITY_THRESHOLD:
                    union(i, j)

        components: dict[int, list[Entry]] = defaultdict(list)
        for i in range(n):
            components[find(i)].append(cat_entries[i])

        for cluster in components.values():
            if len(cluster) >= MIN_CLUSTER_SIZE:
                all_clusters.append(cluster)

    return all_clusters


# ---------------------------------------------------------------------------
# Candidate generation
# ---------------------------------------------------------------------------
def cluster_key(cluster: list[Entry]) -> str:
    """Stable hash key for a cluster (deduplication)."""
    texts = sorted(e.text[:50] for e in cluster)
    return hashlib.sha256("|".join(texts).encode()).hexdigest()[:12]


def generate_rule(cluster: list[Entry]) -> str:
    """Pick the most concise entry as the rule candidate."""
    base = min(cluster, key=lambda e: len(e.text))
    # Strip date annotations: single (2026-03-07) or multi (2026-03-01, 2026-03-07)
    rule = re.sub(r"\s*\(\d{4}-\d{2}-\d{2}(?:,\s*\d{4}-\d{2}-\d{2})*\)\s*$", "", base.text)
    # Strip (imported) tag
    rule = re.sub(r"\s*\(imported\)\s*$", "", rule)
    return rule


# ---------------------------------------------------------------------------
# Pending promotions persistence
# ---------------------------------------------------------------------------
def load_pending() -> dict:
    """Load pending promotions from JSON file."""
    if not PENDING_FILE.exists():
        return {"candidates": [], "processed_clusters": []}
    try:
        data = json.loads(PENDING_FILE.read_text())
        data.setdefault("processed_clusters", [])
        data.setdefault("candidates", [])
        return data
    except (json.JSONDecodeError, OSError):
        return {"candidates": [], "processed_clusters": []}


def save_pending(data: dict) -> None:
    """Save pending promotions atomically."""
    PENDING_FILE.parent.mkdir(parents=True, exist_ok=True)
    tmp = PENDING_FILE.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(data, indent=2))
    tmp.rename(PENDING_FILE)


# ---------------------------------------------------------------------------
# Standing orders helpers
# ---------------------------------------------------------------------------
def get_next_order_num() -> int:
    """Get the next standing order number from CLAUDE.md."""
    if not CLAUDE_MD.exists():
        return 1

    max_num = 0
    in_standing_orders = False

    for line in CLAUDE_MD.read_text().splitlines():
        if line.strip() == "## Standing Orders":
            in_standing_orders = True
        elif line.startswith("## ") and in_standing_orders:
            break
        if in_standing_orders:
            match = re.match(r"\|\s*(\d+)\s*\|", line)
            if match:
                num = int(match.group(1))
                if num > max_num:
                    max_num = num

    return max_num + 1


# ---------------------------------------------------------------------------
# Suggestions file
# ---------------------------------------------------------------------------
def write_suggestion(candidate: dict) -> None:
    """Append a suggestion to evolution/suggestions.md."""
    SUGGESTIONS_FILE.parent.mkdir(parents=True, exist_ok=True)

    today = datetime.now().strftime("%Y-%m-%d")
    dates_str = ", ".join(candidate["dates"][:5])

    suggestion = (
        f"\n## [{today}] Suggestion: Standing order from {candidate['category']}\n"
        f"- **What:** Add standing order: \"{candidate['rule']}\"\n"
        f"- **Why:** Pattern observed {candidate['entry_count']} times ({dates_str})\n"
        f"- **How:** Append to standing orders table in CLAUDE.md\n"
        f"- **Expected benefit:** Consistent behavior without repeated corrections\n"
        f"- **Status:** pending-approval (ID: {candidate['id']})\n"
    )

    existing = SUGGESTIONS_FILE.read_text() if SUGGESTIONS_FILE.exists() else ""
    tmp = SUGGESTIONS_FILE.with_suffix(".md.tmp")
    tmp.write_text(existing + suggestion)
    tmp.rename(SUGGESTIONS_FILE)

    log.info("Wrote suggestion: %s", candidate["id"])


# ---------------------------------------------------------------------------
# Telegram notification
# ---------------------------------------------------------------------------
def _load_telegram_config() -> tuple[str, str]:
    """Load Telegram bot token and chat ID from config."""
    token = os.environ.get("HEX_BOT_TOKEN", "")
    chat_id = ""

    if CONFIG_PATH.exists():
        for line in CONFIG_PATH.read_text().splitlines():
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            if ":" not in line:
                continue
            key, _, val = line.partition(":")
            key = key.strip()
            val = val.strip().strip('"').strip("'")

            if key == "bot_token_env" and not token:
                token = os.environ.get(val, "")
            elif key == "bot_token" and not token:
                env_match = re.match(r"^\$\{(\w+)\}$", val)
                if env_match:
                    token = os.environ.get(env_match.group(1), "")
                else:
                    token = val
            elif key == "chat_id":
                chat_id = val

    return token, chat_id


def send_telegram_notification(candidate: dict) -> bool:
    """Send Telegram notification with Approve/Dismiss inline buttons."""
    token, chat_id = _load_telegram_config()
    if not token or not chat_id:
        log.info("Telegram not configured, skipping notification")
        return False

    url = f"https://api.telegram.org/bot{token}/sendMessage"

    rule_preview = candidate["rule"]
    if len(rule_preview) > 200:
        rule_preview = rule_preview[:200] + "..."

    cid = candidate["id"]

    payload = json.dumps({
        "chat_id": chat_id,
        "text": (
            f"\U0001f4cb New standing order candidate\n\n"
            f"Category: {candidate['category']}\n"
            f"Pattern: {candidate['entry_count']} occurrences\n\n"
            f"{rule_preview}\n\n"
            f"Approve to add as a standing order."
        ),
        "reply_markup": {
            "inline_keyboard": [[
                {"text": "\u2705 Approve", "callback_data": f"promote_approve_{cid}"},
                {"text": "\u274c Dismiss", "callback_data": f"promote_dismiss_{cid}"},
            ]]
        },
    }).encode("utf-8")

    req = urllib.request.Request(
        url, data=payload,
        headers={"Content-Type": "application/json"},
        method="POST",
    )

    try:
        with urllib.request.urlopen(req, timeout=10) as resp:  # nosec B310 — URL scheme is hardcoded Telegram API
            body = json.loads(resp.read().decode("utf-8"))
            ok = body.get("ok", False)
            if ok:
                log.info("Telegram notification sent for %s", cid)
            else:
                log.warning("Telegram API error: %s", body.get("description", ""))
            return ok
    except Exception as e:
        log.warning("Telegram notification failed: %s", e)
        return False


# ---------------------------------------------------------------------------
# Main analysis pipeline
# ---------------------------------------------------------------------------
def analyze_and_promote() -> int:
    """Run the full analysis pipeline. Returns number of new candidates."""
    log.info("Parsing learnings from %s", LEARNINGS_FILE)
    entries = parse_learnings(LEARNINGS_FILE)

    log.info("Parsing reflections from %s", REFLECTIONS_DIR)
    reflection_entries = parse_reflections(REFLECTIONS_DIR)
    entries.extend(reflection_entries)
    log.info("Added %d entries from reflections", len(reflection_entries))

    dated = [e for e in entries if e.date]
    log.info("Found %d total entries (%d dated)", len(entries), len(dated))

    if len(dated) < MIN_CLUSTER_SIZE:
        log.info("Not enough dated entries for clustering")
        return 0

    # Check for single entries that already have 3+ dates (repeated pattern)
    multi_date_entries = [e for e in entries if len(e.dates) >= MIN_CLUSTER_SIZE]
    log.info("Found %d entries with %d+ dates", len(multi_date_entries), MIN_CLUSTER_SIZE)

    clusters = find_clusters(entries)
    log.info("Found %d cluster(s) with %d+ members", len(clusters), MIN_CLUSTER_SIZE)

    # Add multi-date entries as single-entry clusters
    for e in multi_date_entries:
        clusters.append([e])

    if not clusters:
        return 0

    pending = load_pending()
    new_count = 0

    for cluster in clusters:
        ckey = cluster_key(cluster)

        if ckey in pending.get("processed_clusters", []):
            log.info("Cluster %s already processed, skipping", ckey)
            continue

        rule = generate_rule(cluster)
        # Collect all dates from entries (including multi-date entries)
        all_dates: set[str] = set()
        for e in cluster:
            all_dates.update(e.dates)
        dates = sorted(all_dates)

        occurrence_count = max(len(cluster), len(dates))
        candidate: dict[str, Any] = {
            "id": f"promo_{ckey}",
            "category": cluster[0].category,
            "rule": rule,
            "entries": [e.text[:120] for e in cluster],
            "entry_count": occurrence_count,
            "dates": dates,
            "status": "pending",
            "created": datetime.now().strftime("%Y-%m-%d"),
        }

        pending["candidates"].append(candidate)
        pending["processed_clusters"].append(ckey)

        write_suggestion(candidate)
        send_telegram_notification(candidate)

        new_count += 1
        log.info("New candidate: %s [%s]", candidate["id"], candidate["category"])

    if new_count > 0:
        save_pending(pending)

    log.info("Analysis complete. %d new candidate(s)", new_count)
    return new_count


# ---------------------------------------------------------------------------
# Self-test
# ---------------------------------------------------------------------------
def self_test() -> None:
    """Self-test with mock data (no file modifications)."""
    print("=== promote-learnings self-test ===")

    # Stemmer
    cases = [("prefers", "prefer"), ("shorter", "short"), ("requesting", "request"),
             ("fixes", "fix"), ("replies", "reply"), ("consistently", "consistent")]
    for word, expected in cases:
        result = stem(word)
        assert result == expected, f"stem('{word}') = '{result}', expected '{expected}'"
    print("  Stemmer: OK")

    # Tokenizer
    tokens = tokenize("Prefers short concise responses. (2026-03-07)")
    assert "prefer" in tokens, f"Expected 'prefer' in {tokens}"
    assert "short" in tokens, f"Expected 'short' in {tokens}"
    assert "concise" in tokens or "concis" in tokens, f"Expected concise-stem in {tokens}"
    print(f"  Tokenizer: OK ({len(tokens)} tokens)")

    # Jaccard
    assert jaccard({"a", "b", "c"}, {"b", "c", "d"}) == 0.5
    assert jaccard(set(), {"a"}) == 0.0
    assert jaccard({"a"}, {"a"}) == 1.0
    print("  Jaccard: OK")

    # Clustering with mock entries
    mock = [
        Entry("Prefers short concise direct responses. (2026-03-07)", "Comm", "2026-03-07"),
        Entry("Asks for concise short replies without fluff. (2026-03-08)", "Comm", "2026-03-08"),
        Entry("Demands shorter concise output every time. (2026-03-09)", "Comm", "2026-03-09"),
        Entry("Likes coffee in the morning. (2026-03-07)", "Habits", "2026-03-07"),
    ]
    clusters = find_clusters(mock)
    assert len(clusters) == 1, f"Expected 1 cluster, got {len(clusters)}"
    assert len(clusters[0]) == 3, f"Expected cluster size 3, got {len(clusters[0])}"
    print("  Clustering: OK (1 cluster of 3)")

    # Rule generation
    rule = generate_rule(clusters[0])
    assert len(rule) > 10, f"Rule too short: {rule}"
    print("  Rule gen: OK")

    # Cluster key stability
    k1 = cluster_key(clusters[0])
    k2 = cluster_key(clusters[0])
    assert k1 == k2, "Key should be stable"
    print(f"  Cluster key: OK ({k1})")

    # Dedup
    pending = {"candidates": [], "processed_clusters": [k1]}
    assert k1 in pending["processed_clusters"]
    print("  Dedup: OK")

    print("=== All tests passed ===")


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------
if __name__ == "__main__":
    import sys

    if "--test" in sys.argv:
        self_test()
    else:
        analyze_and_promote()
