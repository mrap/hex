#!/usr/bin/env python3
"""fleet-scorecard-aggregate.py — Fleet-level agent performance scorecard.

Discovers all charter.yaml agents, runs agent-performance-review.py for each,
aggregates scores into a fleet scorecard, and sends ONE coalesced Slack digest.

Ergonomics-critic rule (Finding 3, 2026-05-05): NO per-agent pings.
All alerts coalesced into a single message per run.

Usage:
    fleet-scorecard-aggregate.py [--period 7d|14d|30d] [--dry-run] [--output <path>]
"""

import argparse
import json
import os
import re
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

HEX_DIR = Path(__file__).resolve().parents[3]
PROJECTS_DIR = HEX_DIR / "projects"
SCRIPTS_DIR = HEX_DIR / ".hex" / "scripts"
REVIEW_SCRIPT = HEX_DIR / ".hex" / "scripts" / "health" / "agent-performance-review.py"
SLACK_SCRIPT = SCRIPTS_DIR / "slack-post.sh"
COS_DIR = PROJECTS_DIR / "cos"


# ── Agent Discovery ────────────────────────────────────────────────────────────

def discover_agents() -> list[str]:
    """Return agent IDs for all projects that have a charter.yaml."""
    agents = []
    if not PROJECTS_DIR.exists():
        return agents
    for entry in sorted(PROJECTS_DIR.iterdir()):
        if entry.name.startswith("_"):
            continue
        if not entry.is_dir():
            continue
        if (entry / "charter.yaml").exists():
            agents.append(entry.name)
    return agents


# ── Score Parsing ──────────────────────────────────────────────────────────────

def parse_scorecard_output(output: str) -> dict:
    """Extract dimension scores, composite, confidence, and pushback count.

    Expects the markdown output from agent-performance-review.py.
    Format (best-effort — degrades gracefully):
        ## Quality
        ...
        **Score:** 0.72
        ## Velocity
        ...
        **Score:** 0.60
        ## Autonomy
        ...
        **Score:** 0.85
        ## Composite + Trend
        **Composite score:** 0.71
        **Confidence:** normal
        **Mike-pushback count:** 2
    """
    result = {
        "quality": None,
        "velocity": None,
        "autonomy": None,
        "composite": None,
        "confidence": "normal",
        "pushback_count": 0,
        "raw_output": output,
    }

    # Per-section score pattern: **Score:** 0.72 (or Score: 0.72)
    dim_score_re = re.compile(r"\*\*Score:\*\*\s*([\d.]+)", re.IGNORECASE)
    composite_re = re.compile(r"\*\*Composite\s+score:\*\*\s*([\d.]+)", re.IGNORECASE)
    confidence_re = re.compile(r"\*\*Confidence:\*\*\s*(\w+)", re.IGNORECASE)
    pushback_re = re.compile(r"\*\*Mike-pushback\s+count:\*\*\s*(\d+)", re.IGNORECASE)

    # Split by section headers to assign per-dim scores
    sections = re.split(r"^##\s+", output, flags=re.MULTILINE)
    dim_order = ["quality", "velocity", "autonomy"]
    dim_idx = 0
    for section in sections:
        header = section.split("\n", 1)[0].strip().lower()
        score_match = dim_score_re.search(section)
        if not score_match:
            continue
        for dim in dim_order:
            if dim in header and result[dim] is None:
                result[dim] = float(score_match.group(1))
                break
        else:
            # Assign in order if header doesn't match exactly
            if dim_idx < len(dim_order) and result[dim_order[dim_idx]] is None:
                result[dim_order[dim_idx]] = float(score_match.group(1))
                dim_idx += 1

    # Composite
    m = composite_re.search(output)
    if m:
        result["composite"] = float(m.group(1))
    elif all(result[d] is not None for d in dim_order):
        # Derive geometric mean if all three dims present
        q = result["quality"]
        v = result["velocity"]
        a = result["autonomy"]
        if q > 0 and v > 0 and a > 0:
            result["composite"] = (q * v * a) ** (1 / 3)
        else:
            result["composite"] = 0.0

    # Confidence
    m = confidence_re.search(output)
    if m:
        result["confidence"] = m.group(1).lower()

    # Pushback count
    m = pushback_re.search(output)
    if m:
        result["pushback_count"] = int(m.group(1))

    return result


# ── Prior Scorecard Loading (for trend delta) ──────────────────────────────────

def load_prior_composite(agent_id: str, period: str) -> float | None:
    """Read composite from an existing on-disk scorecard for delta computation."""
    scorecard_path = PROJECTS_DIR / agent_id / "scorecards" / f"{period}.md"
    if not scorecard_path.exists():
        return None
    content = scorecard_path.read_text()
    m = re.search(r"\*\*Composite\s+score:\*\*\s*([\d.]+)", content, re.IGNORECASE)
    if m:
        return float(m.group(1))
    return None


# ── Per-Agent Review ───────────────────────────────────────────────────────────

def run_agent_review(agent_id: str, period: str) -> dict | None:
    """Run agent-performance-review.py for one agent, return parsed result."""
    if not REVIEW_SCRIPT.exists():
        return None

    cmd = [
        sys.executable,
        str(REVIEW_SCRIPT),
        "--agent", agent_id,
        "--period", period,
        "--dry-run",
    ]
    try:
        result = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=120,
        )
        output = result.stdout + result.stderr
        parsed = parse_scorecard_output(output)
        parsed["agent_id"] = agent_id
        return parsed
    except subprocess.TimeoutExpired:
        print(f"  [WARN] Timeout running review for {agent_id}", file=sys.stderr)
        return None
    except Exception as e:
        print(f"  [WARN] Error running review for {agent_id}: {e}", file=sys.stderr)
        return None


# ── Fleet Scorecard Composition ────────────────────────────────────────────────

def fmt_score(v: float | None) -> str:
    if v is None:
        return "n/a"
    return f"{v:.2f}"


def fmt_delta(delta: float | None) -> str:
    if delta is None:
        return "—"
    sign = "+" if delta >= 0 else ""
    return f"{sign}{delta:+.2f}"


def build_fleet_scorecard(
    scores: list[dict],
    prior: dict[str, float | None],
    period: str,
    run_date: str,
) -> str:
    """Build the fleet-level scorecard markdown."""
    # Only rank agents with composite score and normal+ confidence
    ranked = [
        s for s in scores
        if s.get("composite") is not None and s.get("confidence") != "low"
    ]
    low_confidence = [
        s for s in scores
        if s.get("confidence") == "low"
    ]
    ranked.sort(key=lambda x: x["composite"], reverse=True)

    # Compute deltas
    for s in scores:
        aid = s["agent_id"]
        p = prior.get(aid)
        if p is not None and s.get("composite") is not None:
            s["delta"] = s["composite"] - p
        else:
            s["delta"] = None

    # Biggest movers (need delta)
    movers = [s for s in ranked if s.get("delta") is not None]
    best_movers = sorted(movers, key=lambda x: x["delta"], reverse=True)[:3]
    worst_movers = sorted(movers, key=lambda x: x["delta"])[:3]

    # Mike-pushback heatmap (all agents, sorted by pushback desc)
    heatmap = sorted(scores, key=lambda x: x.get("pushback_count", 0), reverse=True)

    lines = [
        f"# Fleet Scorecard — {run_date} (period: {period})",
        "",
        f"**Agents scored:** {len(ranked)} (confidence=normal+), "
        f"{len(low_confidence)} cold-start / low-confidence (unranked)",
        "",
    ]

    # Top 5
    lines.append("## Top 5 Performers")
    lines.append("")
    lines.append("| Rank | Agent | Composite | Quality | Velocity | Autonomy | Δ vs Prior |")
    lines.append("|------|-------|:---------:|:-------:|:--------:|:--------:|:----------:|")
    for i, s in enumerate(ranked[:5], 1):
        lines.append(
            f"| {i} | {s['agent_id']} | **{fmt_score(s['composite'])}** | "
            f"{fmt_score(s['quality'])} | {fmt_score(s['velocity'])} | "
            f"{fmt_score(s['autonomy'])} | {fmt_delta(s['delta'])} |"
        )
    lines.append("")

    # Bottom 5
    lines.append("## Bottom 5 Performers")
    lines.append("")
    lines.append("| Rank | Agent | Composite | Quality | Velocity | Autonomy | Δ vs Prior | Confidence |")
    lines.append("|------|-------|:---------:|:-------:|:--------:|:--------:|:----------:|:----------:|")
    bottom = ranked[-5:] if len(ranked) >= 5 else ranked
    bottom_reversed = list(reversed(bottom))
    for i, s in enumerate(bottom_reversed, 1):
        lines.append(
            f"| {i} | {s['agent_id']} | **{fmt_score(s['composite'])}** | "
            f"{fmt_score(s['quality'])} | {fmt_score(s['velocity'])} | "
            f"{fmt_score(s['autonomy'])} | {fmt_delta(s['delta'])} | "
            f"{s.get('confidence', 'normal')} |"
        )
    lines.append("")

    # Low confidence / cold-start
    if low_confidence:
        lines.append("## Cold-Start / Low-Confidence Agents (unranked)")
        lines.append("")
        for s in low_confidence:
            lines.append(f"- **{s['agent_id']}** — confidence=low, insufficient data for ranking")
        lines.append("")

    # Biggest movers
    lines.append("## Biggest Movers vs Prior Period")
    lines.append("")
    if best_movers or worst_movers:
        lines.append("**Most improved:**")
        for s in best_movers:
            lines.append(f"- {s['agent_id']}: {fmt_score(s['composite'])} ({fmt_delta(s['delta'])})")
        lines.append("")
        lines.append("**Biggest regressions:**")
        for s in worst_movers:
            lines.append(f"- {s['agent_id']}: {fmt_score(s['composite'])} ({fmt_delta(s['delta'])})")
    else:
        lines.append("_No prior period data — delta not available._")
    lines.append("")

    # Mike-pushback heatmap
    lines.append("## Mike-Pushback Heatmap")
    lines.append("")
    lines.append("| Agent | Pushback Count |")
    lines.append("|-------|:--------------:|")
    for s in heatmap:
        count = s.get("pushback_count", 0)
        if count > 0:
            lines.append(f"| {s['agent_id']} | {count} |")
    total_pushback = sum(s.get("pushback_count", 0) for s in scores)
    if total_pushback == 0:
        lines.append("_No Mike-pushback signals in this period._")
    lines.append("")

    # Generation metadata
    lines.append("---")
    lines.append(f"_Generated: {run_date} | Period: {period} | Script: fleet-scorecard-aggregate.py_")
    lines.append("")

    return "\n".join(lines)


# ── Slack Digest (coalesced — one message) ─────────────────────────────────────

def send_slack_digest(
    ranked: list[dict],
    n_regressions: int,
    n_improvements: int,
    scorecard_path: Path,
) -> None:
    """Send ONE coalesced fleet digest to configured Slack channel.

    Ergonomics rule: single message, no per-agent pings.
    """
    if not SLACK_SCRIPT.exists():
        print("[INFO] slack-post.sh not found — skipping Slack digest", file=sys.stderr)
        return

    top_agent = ranked[0]["agent_id"] if ranked else "n/a"
    top_score = fmt_score(ranked[0]["composite"]) if ranked else "n/a"
    bottom_agent = ranked[-1]["agent_id"] if len(ranked) > 1 else "n/a"
    bottom_score = fmt_score(ranked[-1]["composite"]) if len(ranked) > 1 else "n/a"
    run_date = datetime.now(timezone.utc).strftime("%Y-%m-%d")

    msg = (
        f"Fleet scorecard {run_date}: "
        f"top={top_agent} {top_score}; "
        f"bottom={bottom_agent} {bottom_score}; "
        f"{n_regressions} regression(s), {n_improvements} improvement(s). "
        f"Full: {scorecard_path}"
    )

    try:
        subprocess.run(
            [str(SLACK_SCRIPT), "WARN", "default", msg],
            timeout=30,
            check=False,
        )
    except Exception as e:
        print(f"[WARN] Slack digest failed: {e}", file=sys.stderr)


# ── Main ───────────────────────────────────────────────────────────────────────

def main() -> None:
    parser = argparse.ArgumentParser(description="Fleet-level agent performance scorecard")
    parser.add_argument("--period", default="7d", choices=["7d", "14d", "30d"])
    parser.add_argument("--dry-run", action="store_true",
                        help="Build scorecard but do not write files or send Slack")
    parser.add_argument("--output", default=None,
                        help="Override output path for the fleet scorecard")
    parser.add_argument("--no-slack", action="store_true",
                        help="Skip Slack digest even when not --dry-run")
    args = parser.parse_args()

    run_date = datetime.now(timezone.utc).strftime("%Y-%m-%d")
    period = args.period

    # ── Discover agents ───────────────────────────────────────────────────────
    agents = discover_agents()
    if not agents:
        print("No agents discovered in projects/ — nothing to score.", file=sys.stderr)
        sys.exit(0)

    print(f"[fleet-scorecard] {len(agents)} agents discovered, period={period}", file=sys.stderr)

    if not REVIEW_SCRIPT.exists():
        print(
            f"[WARN] agent-performance-review.py not found at {REVIEW_SCRIPT}\n"
            "       Fleet scorecard will use stub scores. Install TFF0E to enable real scoring.",
            file=sys.stderr,
        )

    # ── Load prior composites for delta ───────────────────────────────────────
    prior: dict[str, float | None] = {}
    for agent_id in agents:
        prior[agent_id] = load_prior_composite(agent_id, period)

    # ── Run per-agent reviews ─────────────────────────────────────────────────
    scores: list[dict] = []
    for agent_id in agents:
        print(f"  [fleet-scorecard] scoring {agent_id}...", file=sys.stderr)
        result = run_agent_review(agent_id, period)
        if result is None:
            # Stub entry — not enough data to score
            result = {
                "agent_id": agent_id,
                "quality": None,
                "velocity": None,
                "autonomy": None,
                "composite": None,
                "confidence": "low",
                "pushback_count": 0,
            }
        scores.append(result)

    # ── Build fleet scorecard ─────────────────────────────────────────────────
    content = build_fleet_scorecard(scores, prior, period, run_date)

    # ── Compute improvement / regression counts for Slack ─────────────────────
    n_improvements = sum(
        1 for s in scores
        if s.get("delta") is not None and s["delta"] > 0.05
    )
    n_regressions = sum(
        1 for s in scores
        if s.get("delta") is not None and s["delta"] < -0.05
    )

    # ── Determine output path ─────────────────────────────────────────────────
    if args.output:
        out_path = Path(args.output)
    else:
        out_path = COS_DIR / f"fleet-scorecard-{run_date}-{period}.md"

    # ── Write or print ────────────────────────────────────────────────────────
    if args.dry_run:
        print(content)
        print(f"\n[dry-run] Would write to: {out_path}", file=sys.stderr)
    else:
        out_path.parent.mkdir(parents=True, exist_ok=True)
        tmp_path = out_path.with_suffix(".md.tmp")
        tmp_path.write_text(content)
        tmp_path.rename(out_path)
        print(f"[fleet-scorecard] Written: {out_path}", file=sys.stderr)

        # Update latest.md symlink in cos/
        latest_path = COS_DIR / "fleet-scorecard-latest.md"
        try:
            if latest_path.exists() or latest_path.is_symlink():
                latest_path.unlink()
            latest_path.symlink_to(out_path.name)
        except Exception as e:
            print(f"[WARN] Could not update latest symlink: {e}", file=sys.stderr)

    # ── Slack digest (coalesced, NOT per-agent) ───────────────────────────────
    ranked = [s for s in scores if s.get("composite") is not None and s.get("confidence") != "low"]
    ranked.sort(key=lambda x: x["composite"], reverse=True)

    if not args.dry_run and not args.no_slack:
        send_slack_digest(ranked, n_regressions, n_improvements, out_path)
    elif args.dry_run:
        top_agent = ranked[0]["agent_id"] if ranked else "n/a"
        top_score = fmt_score(ranked[0]["composite"]) if ranked else "n/a"
        bottom_agent = ranked[-1]["agent_id"] if len(ranked) > 1 else "n/a"
        bottom_score = fmt_score(ranked[-1]["composite"]) if len(ranked) > 1 else "n/a"
        print(
            f"\n[dry-run] Slack digest would send: "
            f"top={top_agent} {top_score}; bottom={bottom_agent} {bottom_score}; "
            f"{n_regressions} regression(s), {n_improvements} improvement(s)",
            file=sys.stderr,
        )


if __name__ == "__main__":
    main()
