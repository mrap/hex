#!/usr/bin/env bash
# synthesis-trigger.sh — Cluster related inputs and dispatch synthesis BOI specs.
#
# Scans captures and project context files from the last 7 days, clusters related
# items by topic, and generates BOI specs for any cluster with 3+ items.
# Skips topics that already have a synthesis output from the last 7 days.
#
# Usage:
#   synthesis-trigger.sh              # run with defaults
#   synthesis-trigger.sh --dry-run    # show what would be dispatched
#   synthesis-trigger.sh --days N     # override lookback window (default: 7)

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE="$(cd "$SCRIPT_DIR/../.." && pwd)"
CAPTURES_DIR="$WORKSPACE/raw/captures"
PROJECTS_DIR="$WORKSPACE/projects"
RESEARCH_DIR="$WORKSPACE/raw/research"
BOI="$HOME/.boi/boi"
SPEC_STAGING_DIR="$CAPTURES_DIR/.dispatch-staging"
DRY_RUN=false
LOOKBACK_DAYS=7

# Parse args
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run) DRY_RUN=true; shift ;;
    --days)    LOOKBACK_DAYS="$2"; shift 2 ;;
    *) echo "Unknown arg: $1"; exit 1 ;;
  esac
done

echo "=== synthesis-trigger ==="
echo "Workspace: $WORKSPACE"
echo "Lookback:  ${LOOKBACK_DAYS} days"
echo "Dry run:   $DRY_RUN"
echo ""

mkdir -p "$SPEC_STAGING_DIR"
mkdir -p "$RESEARCH_DIR"

# Run Python clustering and spec generation
python3 - \
  "$WORKSPACE" \
  "$CAPTURES_DIR" \
  "$PROJECTS_DIR" \
  "$RESEARCH_DIR" \
  "$SPEC_STAGING_DIR" \
  "$LOOKBACK_DAYS" \
  "$DRY_RUN" \
  "$BOI" \
<<'PYEOF'
import sys
import os
import re
import json
import time
import subprocess
from pathlib import Path
from collections import defaultdict
from datetime import datetime, timedelta, timezone

(workspace, captures_dir, projects_dir, research_dir,
 spec_staging_dir, lookback_days_str, dry_run_str, boi_bin) = sys.argv[1:]

WORKSPACE     = Path(workspace)
CAPTURES_DIR  = Path(captures_dir)
PROJECTS_DIR  = Path(projects_dir)
RESEARCH_DIR  = Path(research_dir)
STAGING_DIR   = Path(spec_staging_dir)
LOOKBACK_DAYS = int(lookback_days_str)
DRY_RUN       = dry_run_str == "true"
BOI           = boi_bin

TODAY     = datetime.now().strftime("%Y-%m-%d")
CUTOFF_TS = time.time() - LOOKBACK_DAYS * 86400

# Topic keyword lists — each entry is (topic_id, human_name, keywords[])
TOPICS = [
    ("job-search", "Job Search & Career", [
        "job", "scout", "career", "hiring", "recruiter", "interview", "comp",
        "salary", "role", "staff", "engineer", "resume", "linkedin", "opportunity",
        "company", "pipeline", "warm", "referral", "application"
    ]),
    ("investments", "Investments & Portfolio", [
        "invest", "portfolio", "stock", "market", "trading", "crypto", "financial",
        "fintech", "asset", "yield", "defi", "token", "quant", "alpha", "ticker",
        "fund", "etf", "equity", "rsu", "espp", "allocation", "rebalance"
    ]),
    ("ai-agents", "AI Agents & Automation", [
        "agent", "boi", "hex", "automation", "harness", "worker", "spec",
        "dispatch", "orchestrat", "claude", "llm", "autonomous", "loop",
        "workflow", "fleet", "skill", "hook", "trigger", "pipeline"
    ]),
    ("ai-models", "AI Models & Research", [
        "model", "llm", "inference", "context", "benchmark", "eval",
        "gemini", "openai", "anthropic", "sonnet", "opus", "haiku",
        "gpt", "training", "finetune", "parameter", "token", "embedding"
    ]),
    ("product-engineering", "Product & Engineering", [
        "product", "feature", "refactor", "bug", "deploy", "release",
        "architecture", "design", "api", "schema", "database", "migration",
        "performance", "scale", "test", "review", "pr", "branch"
    ]),
]

STOPWORDS = {
    "the", "a", "an", "and", "or", "but", "in", "on", "at", "to", "for",
    "of", "with", "by", "from", "is", "are", "was", "were", "be", "been",
    "have", "has", "had", "do", "does", "did", "will", "would", "could",
    "should", "may", "might", "can", "this", "that", "these", "those",
    "it", "its", "not", "as", "if", "so", "up", "out", "into", "about",
    "new", "more", "also", "just", "all", "one", "two", "three", "hex",
    "mike", "mrap", "md", "yaml", "json",
}

def extract_text(path: Path) -> str:
    """Return first 1000 chars of file content, stripping YAML frontmatter."""
    try:
        raw = path.read_text(errors="replace")
    except OSError:
        return ""
    # Strip frontmatter
    if raw.startswith("---"):
        end = raw.find("\n---", 3)
        if end != -1:
            raw = raw[end + 4:]
    return raw[:1000]

def keywords_from(path: Path) -> set[str]:
    """Extract lowercase keywords from filename + content."""
    name_words = set(re.split(r"[-_./]", path.stem.lower()))
    content = extract_text(path)
    content_words = set(re.findall(r"[a-z]{3,}", content.lower()))
    words = (name_words | content_words) - STOPWORDS
    return words

def score_topic(words: set[str], topic_kws: list[str]) -> int:
    """Count how many topic keywords appear in the word set."""
    score = 0
    for kw in topic_kws:
        kw_lower = kw.lower()
        for w in words:
            if kw_lower in w or w in kw_lower:
                score += 1
                break
    return score

def best_topic(words: set[str]) -> tuple[str, str] | None:
    """Return (topic_id, topic_name) with highest score, or None if score < 2."""
    best_id, best_name, best_score = None, None, 0
    for topic_id, topic_name, kws in TOPICS:
        s = score_topic(words, kws)
        if s > best_score:
            best_id, best_name, best_score = topic_id, topic_name, s
    if best_score < 2:
        return None
    return best_id, best_name

def recent_synthesis_exists(topic_id: str) -> bool:
    """True if a synthesis output for this topic was written within last 7 days."""
    pattern = re.compile(rf"{re.escape(topic_id)}-synthesis-\d{{4}}-\d{{2}}-\d{{2}}\.md$")
    for p in RESEARCH_DIR.glob(f"{topic_id}-synthesis-*.md"):
        if pattern.match(p.name) and p.stat().st_mtime >= CUTOFF_TS:
            return True
    return False

# --- Collect recent files ---
items = []  # list of (Path, label)

# Recent captures
if CAPTURES_DIR.is_dir():
    for p in CAPTURES_DIR.glob("*.md"):
        if p.name.startswith("TRIAGE-"):
            continue
        if p.stat().st_mtime >= CUTOFF_TS:
            items.append((p, "capture"))

# Recent project context files
if PROJECTS_DIR.is_dir():
    for ctx in PROJECTS_DIR.glob("*/context.md"):
        if ctx.stat().st_mtime >= CUTOFF_TS:
            items.append((ctx, f"context:{ctx.parent.name}"))

print(f"[synthesis-trigger] Scanned {len(items)} recent files (captures + context)")

# --- Cluster by topic ---
clusters: dict[str, dict] = {}  # topic_id -> {name, files:[]}
for path, label in items:
    words = keywords_from(path)
    result = best_topic(words)
    if result is None:
        continue
    topic_id, topic_name = result
    if topic_id not in clusters:
        clusters[topic_id] = {"name": topic_name, "files": []}
    clusters[topic_id]["files"].append((path, label))

# --- Report ---
print(f"\n[synthesis-trigger] Cluster results:")
for tid, info in sorted(clusters.items()):
    flag = ""
    if len(info["files"]) >= 3:
        flag = " ← ELIGIBLE" if not recent_synthesis_exists(tid) else " ← SKIP (recent synthesis exists)"
    print(f"  {tid}: {len(info['files'])} items{flag}")

# --- Generate specs for eligible clusters ---
dispatched = 0
for topic_id, info in sorted(clusters.items()):
    files = info["files"]
    if len(files) < 3:
        continue
    if recent_synthesis_exists(topic_id):
        print(f"\n[synthesis-trigger] Skipping {topic_id} — synthesis already done this week")
        continue

    topic_name = info["name"]
    output_path = RESEARCH_DIR / f"{topic_id}-synthesis-{TODAY}.md"
    spec_path   = STAGING_DIR / f"synthesis-{topic_id}-{TODAY}.spec.md"

    # Build file list for spec
    file_lines = "\n".join(
        f"  - `{p}` ({label})" for p, label in files[:20]
    )

    spec_content = f"""# Synthesize: {topic_name}

**Mode:** generate
**Workspace:** {workspace}

## Goal

Synthesize {len(files)} related inputs about **{topic_name}** collected over the last {LOOKBACK_DAYS} days.
Identify patterns, consolidate findings, and produce a concise actionable report.

## Source Files

{file_lines}

## Constraints

- Output must be written to: `{output_path}`
- Length: 300–800 words. Signal-dense, not exhaustive.
- Format: brief intro, key findings (bullet list), patterns/themes, recommended actions.
- Do not simply summarize each file individually — synthesize across them.
- Use only information present in the source files. Do not hallucinate.

## Success Criteria

- Output file exists at `{output_path}`
- Contains at least 3 key findings
- Contains at least 1 recommended action
- Does not exceed 1000 words

## Tasks

### t-1: Synthesize {topic_name} inputs
PENDING

**Spec:** Read each source file listed above. Identify common themes, patterns,
and insights across the {len(files)} inputs. Write a synthesis report to:
`{output_path}`

Structure the report as:
1. **Summary** (2-3 sentences)
2. **Key Findings** (3-7 bullet points)
3. **Patterns** (recurring themes across inputs)
4. **Recommended Actions** (concrete next steps for Mike)

**Verify:**
```bash
test -f "{output_path}" && wc -w "{output_path}"
```
"""

    print(f"\n[synthesis-trigger] Cluster: {topic_id} ({len(files)} items)")
    print(f"  Output: {output_path}")
    print(f"  Spec:   {spec_path.name}")

    if DRY_RUN:
        print(f"  [DRY RUN] Would write spec and dispatch")
        dispatched += 1
        continue

    # Atomic write
    tmp = Path(str(spec_path) + ".tmp")
    tmp.write_text(spec_content)
    tmp.rename(spec_path)

    # Dispatch
    result = subprocess.run(
        [BOI, "dispatch", str(spec_path), "--mode", "generate", "--no-critic"],
        capture_output=True, text=True
    )
    if result.returncode == 0:
        print(f"  Dispatched: {spec_path.name}")
        dispatched += 1
    else:
        print(f"  DISPATCH FAILED: {result.stderr.strip()[:200]}")

print(f"\n[synthesis-trigger] Done — {dispatched} synthesis spec(s) dispatched")
PYEOF
