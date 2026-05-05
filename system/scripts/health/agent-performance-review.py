#!/usr/bin/env python3
"""
agent-performance-review.py — score quality + velocity + autonomy per agent

Critic findings applied from TAAB8 (quality-antagonist + ergonomics-critic):
  - Weighted sum + sparse-data handling (NOT geometric mean)
  - Archetype split: initiative-driver vs reactive-critic
  - Cold-start gate: < 5 wakes → confidence=insufficient, no rank
  - Blocked-resolution rate replaces wake-act-rate
  - Only explicit retractions count as junk (not N-hour re-edits)
  - Mike pushback excluded from quality (can't type-bin without structured tagging)
  - Useful-artifact ratio only used for 30d windows
  - No individual signal values injected into prompts (Goodhart prevention)

Usage:
  agent-performance-review.py --agent boi-optimizer --period 7d --dry-run
  agent-performance-review.py --all --period 30d --output /tmp/scores/
"""

import argparse
import json
import math
import os
import re
import sqlite3
import sys
import yaml
from datetime import datetime, timedelta, timezone
from pathlib import Path

# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------
HEX_DIR = Path(os.environ.get("HEX_DIR", Path.home() / "mrap-hex"))
PROJECTS_DIR = HEX_DIR / "projects"
BOI_DB = Path.home() / ".boi" / "boi-rust.db"
MESSAGES_PATH = HEX_DIR / ".hex" / "data" / "messages.json"
MIKE_PENDING_PATH = PROJECTS_DIR / "cos" / "mike-pending.jsonl"

# Reactive-critic archetype: agents whose velocity is responsiveness-based
REACTIVE_CRITIC_IDS = {
    "anti-pattern-hunter",
    "ergonomics-critic",
    "system-arch-critic",
    "quality-antagonist",
}

# Corrective sentiment keywords (factually-wrong class only per TAAB8)
CORRECTIVE_KEYWORDS = [
    "cringe", "stalled", "stop", "lame", "fix this", "broken",
    "regression", "ghost-wake", "junk", "wrong", "incorrect",
    "that's wrong", "not right", "bad output", "bad work",
]

# Cold-start gate thresholds (from quality-antagonist review)
COLD_START_MIN_WAKES = 5
COLD_START_MIN_ACTS = 3

# Score weights by archetype
WEIGHTS_INITIATIVE = {"quality": 0.40, "velocity": 0.35, "autonomy": 0.25}
WEIGHTS_REACTIVE = {"quality": 0.60, "responsiveness": 0.40}


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def parse_period(period_str: str) -> int:
    """Return number of days for '7d', '14d', '30d'."""
    m = re.match(r"^(\d+)d$", period_str)
    if not m:
        raise ValueError(f"Invalid period: {period_str}. Use 7d, 14d, or 30d.")
    return int(m.group(1))


def window_start(days: int) -> datetime:
    return datetime.now(timezone.utc) - timedelta(days=days)


def parse_ts(ts_str: str) -> datetime | None:
    if not ts_str:
        return None
    for fmt in ("%Y-%m-%dT%H:%M:%S.%fZ", "%Y-%m-%dT%H:%M:%SZ",
                "%Y-%m-%dT%H:%M:%S%z", "%Y-%m-%dT%H:%M:%S.%f%z"):
        try:
            dt = datetime.strptime(ts_str, fmt)
            if dt.tzinfo is None:
                dt = dt.replace(tzinfo=timezone.utc)
            return dt
        except ValueError:
            continue
    return None


def in_window(ts_str: str, since: datetime) -> bool:
    dt = parse_ts(ts_str)
    if dt is None:
        return False
    return dt >= since


def load_state(agent_id: str) -> dict:
    state_path = PROJECTS_DIR / agent_id / "state.json"
    if not state_path.exists():
        return {}
    try:
        return json.loads(state_path.read_text())
    except Exception:
        return {}


def load_charter(agent_id: str) -> dict:
    charter_path = PROJECTS_DIR / agent_id / "charter.yaml"
    if not charter_path.exists():
        return {}
    try:
        return yaml.safe_load(charter_path.read_text()) or {}
    except Exception:
        return {}


def load_messages() -> list:
    if not MESSAGES_PATH.exists():
        return []
    try:
        data = json.loads(MESSAGES_PATH.read_text())
        return data.get("messages", []) if isinstance(data, dict) else []
    except Exception:
        return []


def load_mike_pending() -> list:
    if not MIKE_PENDING_PATH.exists():
        return []
    items = []
    for line in MIKE_PENDING_PATH.read_text().splitlines():
        line = line.strip()
        if line:
            try:
                items.append(json.loads(line))
            except Exception:
                pass
    return items


def is_reactive_critic(agent_id: str, charter: dict) -> bool:
    if agent_id in REACTIVE_CRITIC_IDS:
        return True
    if "arch_pipeline_critic" in charter:
        return True
    role = charter.get("role", "").lower()
    return any(w in role for w in ("critic", "auditor", "antagonist", "on-call"))


def list_all_agents() -> list[str]:
    """Return agent IDs that have charter.yaml or state.json."""
    agents = []
    if not PROJECTS_DIR.exists():
        return agents
    for p in sorted(PROJECTS_DIR.iterdir()):
        if p.is_dir() and not p.name.startswith("_"):
            if (p / "state.json").exists() or (p / "charter.yaml").exists():
                agents.append(p.name)
    return agents


# ---------------------------------------------------------------------------
# Signal extraction
# ---------------------------------------------------------------------------

def extract_trail_in_window(trail: list, since: datetime) -> list:
    return [e for e in trail if in_window(e.get("ts", ""), since)]


def count_trail_by_type(trail_in_window: list, type_name: str) -> int:
    return sum(1 for e in trail_in_window if e.get("type") == type_name)


def extract_quality_signals_initiative(
    agent_id: str, trail_w: list, messages: list, mike_pending: list,
    since: datetime, period_days: int
) -> dict:
    """Quality signals for initiative-driver agents."""
    signals = {}

    # 1. Critic review mentions — search all critic-agent review files
    high_count = 0
    med_count = 0
    low_count = 0
    critic_review_dirs = [
        PROJECTS_DIR / "quality-antagonist" / "reviews",
        PROJECTS_DIR / "anti-pattern-hunter" / "reviews",
        PROJECTS_DIR / "ergonomics-critic" / "reviews",
        PROJECTS_DIR / "system-arch-critic" / "reviews",
    ]
    name_pattern = re.compile(re.escape(agent_id), re.IGNORECASE)
    for review_dir in critic_review_dirs:
        if not review_dir.exists():
            continue
        for review_file in review_dir.glob("*.md"):
            try:
                stat = review_file.stat()
                mtime = datetime.fromtimestamp(stat.st_mtime, tz=timezone.utc)
                if mtime < since:
                    continue
                text = review_file.read_text()
                if not name_pattern.search(text):
                    continue
                # Count HIGH/MED/LOW near agent name
                lines = text.splitlines()
                for i, line in enumerate(lines):
                    context = "\n".join(lines[max(0, i-2):min(len(lines), i+3)])
                    if name_pattern.search(line):
                        if "HIGH" in context or "CRITICAL" in context:
                            high_count += 1
                        elif "MED" in context or "MEDIUM" in context:
                            med_count += 1
                        elif "LOW" in context:
                            low_count += 1
            except Exception:
                pass
    signals["critic_high"] = high_count
    signals["critic_med"] = med_count
    signals["critic_low"] = low_count

    # 2. Corrective messages from Mike/hex-main to this agent in window
    corrective_count = 0
    for msg in messages:
        if not in_window(msg.get("sent_at") or msg.get("created_at", ""), since):
            continue
        sender = (msg.get("from") or "").lower()
        if sender not in ("mike", "hex-main", "ops"):
            continue
        recipients = msg.get("to", [])
        if agent_id not in [str(r).lower() for r in recipients]:
            continue
        content = (msg.get("content") or "").lower()
        if any(kw in content for kw in CORRECTIVE_KEYWORDS):
            corrective_count += 1
    signals["corrective_msgs"] = corrective_count

    # 3. Explicit retractions (trail entries mentioning retract/reverted)
    retraction_keywords = ["retract", "reverted", "revert", "superseded", "rolling back"]
    retraction_count = 0
    for e in trail_w:
        detail_str = json.dumps(e.get("detail", "")).lower()
        if any(kw in detail_str for kw in retraction_keywords):
            retraction_count += 1
    signals["retraction_count"] = retraction_count

    # Exclude useful-artifact ratio unless period >= 30d (temporal confound <30d)
    if period_days >= 30:
        signals["useful_artifact_ratio"] = None  # placeholder — requires cross-agent ref tracking

    return signals


def extract_quality_signals_reactive(
    agent_id: str, trail_w: list, since: datetime
) -> dict:
    """Quality signals for reactive-critic agents."""
    signals = {}

    # 1. Review files created in window
    review_dir = PROJECTS_DIR / agent_id / "reviews"
    review_count = 0
    high_findings = 0
    med_findings = 0
    low_findings = 0
    if review_dir.exists():
        for f in review_dir.glob("*.md"):
            try:
                mtime = datetime.fromtimestamp(f.stat().st_mtime, tz=timezone.utc)
                if mtime < since:
                    continue
                review_count += 1
                text = f.read_text()
                high_findings += len(re.findall(r"\b(HIGH|CRITICAL)\b", text))
                med_findings += len(re.findall(r"\b(MED|MEDIUM)\b", text))
                low_findings += len(re.findall(r"\bLOW\b", text))
            except Exception:
                pass
    signals["review_count"] = review_count
    signals["high_findings"] = high_findings
    signals["med_findings"] = med_findings
    signals["low_findings"] = low_findings

    return signals


def extract_velocity_signals_initiative(
    agent_id: str, trail_w: list, since: datetime
) -> dict:
    """Velocity signals for initiative-driver agents."""
    signals = {}

    # 1. Act count in window (actions taken)
    signals["act_count"] = count_trail_by_type(trail_w, "act")

    # 2. Initiative tracking file updated in window
    init_tracking = PROJECTS_DIR / agent_id / "initiative-tracking.md"
    if init_tracking.exists():
        mtime = datetime.fromtimestamp(init_tracking.stat().st_mtime, tz=timezone.utc)
        signals["initiative_tracking_updated"] = mtime >= since
        signals["initiative_tracking_age_days"] = (
            datetime.now(timezone.utc) - mtime
        ).days
    else:
        signals["initiative_tracking_updated"] = False
        signals["initiative_tracking_age_days"] = None

    # 3. Inbox items processed: trail entries that reference inbox and are act/verify
    inbox_processed = 0
    for e in trail_w:
        if e.get("type") in ("act", "verify"):
            detail_str = json.dumps(e.get("detail", "")).lower()
            if "inbox" in detail_str or "queue_item" in str(e.get("queue_item", "")).lower():
                inbox_processed += 1
        if e.get("queue_item"):
            if e.get("type") in ("act", "verify"):
                inbox_processed += 1
    signals["inbox_processed"] = inbox_processed

    # 4. Message_sent count (outgoing communication)
    signals["messages_sent"] = count_trail_by_type(trail_w, "message_sent")

    return signals


def extract_velocity_signals_reactive(
    agent_id: str, trail_w: list, since: datetime
) -> dict:
    """Velocity (responsiveness) signals for reactive-critic agents."""
    signals = {}

    review_dir = PROJECTS_DIR / agent_id / "reviews"
    review_count = 0
    if review_dir.exists():
        for f in review_dir.glob("*.md"):
            try:
                mtime = datetime.fromtimestamp(f.stat().st_mtime, tz=timezone.utc)
                if mtime >= since:
                    review_count += 1
            except Exception:
                pass
    signals["reviews_in_period"] = review_count
    signals["act_count"] = count_trail_by_type(trail_w, "act")

    return signals


def extract_autonomy_signals(
    agent_id: str, trail_w: list, messages: list, mike_pending: list, since: datetime
) -> dict:
    """Autonomy signals (both archetypes — reactive critics have low autonomy surface)."""
    signals = {}

    # 1. Mike-pending items registered by this agent in window
    pending_count = 0
    for item in mike_pending:
        if item.get("registered_by", "").lower() == agent_id.lower():
            reg_ts = item.get("registered_at", "")
            if in_window(reg_ts, since):
                pending_count += 1
    signals["mike_pending_count"] = pending_count

    # 2. Act / Park counts for blocked-resolution estimation
    act_count = count_trail_by_type(trail_w, "act")
    park_count = count_trail_by_type(trail_w, "park")
    signals["act_count"] = act_count
    signals["park_count"] = park_count

    # Blocked-resolution rate: proxy — parks that have a resume_condition (structured block)
    # vs vague parks. Structured parks are legitimate; vague parks may be avoidance.
    legitimate_parks = 0
    avoidance_parks = 0
    for e in trail_w:
        if e.get("type") != "park":
            continue
        detail = e.get("detail", {})
        if isinstance(detail, dict):
            has_resume = bool(detail.get("resume_condition"))
            has_reason = bool(detail.get("reason"))
            if has_resume or (has_reason and len(str(detail.get("reason", ""))) > 20):
                legitimate_parks += 1
            else:
                avoidance_parks += 1
    signals["legitimate_parks"] = legitimate_parks
    signals["avoidance_parks"] = avoidance_parks

    # 3. response_requested rate: trail entries or detail text mentioning it
    rr_count = 0
    msg_sent_count = 0
    for e in trail_w:
        if e.get("type") in ("act", "message_sent"):
            msg_sent_count += 1
            detail_str = json.dumps(e.get("detail", "")).lower()
            if "response_requested" in detail_str and (
                ": true" in detail_str or "=true" in detail_str or "response_requested: true" in detail_str
            ):
                rr_count += 1
    signals["response_requested_count"] = rr_count
    signals["message_sent_count"] = msg_sent_count

    return signals


# ---------------------------------------------------------------------------
# Score computation (weighted sum with sparse-data handling)
# ---------------------------------------------------------------------------

def score_quality_initiative(signals: dict) -> tuple[float, float, list[str]]:
    """Returns (score 0-1, confidence 0-1, notes)."""
    subscores = {}
    notes = []

    # Critic review mentions
    h = signals.get("critic_high", 0)
    m_count = signals.get("critic_med", 0)
    l = signals.get("critic_low", 0)
    critic_score = max(0.0, 1.0 - (h * 0.25) - (m_count * 0.10) - (l * 0.03))
    subscores["critic_mentions"] = (critic_score, 0.35)
    if h > 0:
        notes.append(f"{h} HIGH findings in critic reviews")

    # Corrective messages
    c = signals.get("corrective_msgs", 0)
    corr_score = max(0.0, 1.0 - (c * 0.20))
    subscores["corrective_msgs"] = (corr_score, 0.35)
    if c > 0:
        notes.append(f"{c} corrective messages from Mike/hex-main")

    # Explicit retractions
    r = signals.get("retraction_count", 0)
    ret_score = max(0.0, 1.0 - (r * 0.30))
    subscores["retractions"] = (ret_score, 0.30)
    if r > 0:
        notes.append(f"{r} explicit retractions in trail")

    # Weighted sum
    total_weight = sum(w for _, w in subscores.values())
    if total_weight == 0:
        return 0.5, 0.0, ["No quality signals found"]
    score = sum(s * w for s, w in subscores.values()) / total_weight
    confidence = min(1.0, total_weight)
    return round(score, 3), round(confidence, 3), notes


def score_quality_reactive(signals: dict) -> tuple[float, float, list[str]]:
    """Quality score for reactive-critic agents."""
    subscores = {}
    notes = []

    # Review output in period
    rc = signals.get("review_count", 0)
    high = signals.get("high_findings", 0)
    output_score = min(1.0, rc / 2.0)  # 2+ reviews = 1.0
    subscores["review_output"] = (output_score, 0.40)

    # Finding density (HIGH findings per review = substantive quality)
    if rc > 0:
        density = high / rc
        density_score = min(1.0, density / 3.0)  # 3+ HIGH per review = 1.0
        subscores["finding_density"] = (density_score, 0.60)
        notes.append(f"{rc} reviews, {high} HIGH findings in period")
    else:
        notes.append("No reviews in period (agent may not have been invoked)")

    if not subscores:
        return 0.5, 0.0, notes
    total_weight = sum(w for _, w in subscores.values())
    score = sum(s * w for s, w in subscores.values()) / total_weight
    confidence = min(1.0, rc / 1.0)  # At least 1 review for any confidence
    return round(score, 3), round(confidence, 3), notes


def score_velocity_initiative(signals: dict, period_days: int) -> tuple[float, float, list[str]]:
    """Velocity score for initiative-driver agents."""
    subscores = {}
    notes = []

    # Act count: expected ~1 act per day is baseline
    act = signals.get("act_count", 0)
    expected_acts = period_days * 1.0
    act_score = min(1.0, act / max(1, expected_acts))
    subscores["act_count"] = (act_score, 0.50)
    notes.append(f"{act} actions in {period_days}d window")

    # Initiative tracking freshness
    updated = signals.get("initiative_tracking_updated", False)
    age = signals.get("initiative_tracking_age_days")
    if age is not None:
        freshness = 1.0 if updated else max(0.0, 1.0 - age / 14.0)
        subscores["initiative_freshness"] = (freshness, 0.30)
        if not updated:
            notes.append(f"initiative-tracking.md not updated in period ({age}d old)")
    else:
        notes.append("No initiative-tracking.md found")

    # Inbox processing
    inbox = signals.get("inbox_processed", 0)
    inbox_score = min(1.0, inbox / max(1, period_days * 0.5))
    subscores["inbox_processed"] = (inbox_score, 0.20)

    if not subscores:
        return 0.0, 0.0, notes
    total_weight = sum(w for _, w in subscores.values())
    score = sum(s * w for s, w in subscores.values()) / total_weight
    confidence = 1.0 if act > 0 else 0.3
    return round(score, 3), round(confidence, 3), notes


def score_velocity_reactive(signals: dict, period_days: int) -> tuple[float, float, list[str]]:
    """Velocity (responsiveness) score for reactive-critic agents."""
    notes = []
    reviews = signals.get("reviews_in_period", 0)
    act = signals.get("act_count", 0)

    # Reactive agents score on reviews produced (already in quality) + responsiveness proxy
    # Use combined act + review count normalized to period
    combined = act + reviews
    score = min(1.0, combined / max(1, period_days * 0.5))
    confidence = 1.0 if combined > 0 else 0.2
    notes.append(f"{reviews} reviews, {act} actions in period")
    return round(score, 3), round(confidence, 3), notes


def score_autonomy(signals: dict, is_reactive: bool) -> tuple[float, float, list[str]]:
    """Autonomy score (lower Mike dependency = better)."""
    subscores = {}
    notes = []

    # Escalation rate (mike-pending / act_count)
    pending = signals.get("mike_pending_count", 0)
    act = signals.get("act_count", 0)
    if act > 0:
        esc_rate = pending / act
        esc_score = max(0.0, 1.0 - esc_rate * 5.0)  # 20% escalation rate = 0.0
        subscores["escalation_rate"] = (esc_score, 0.35)
        if pending > 0:
            notes.append(f"{pending} Mike-pending items registered in period")
    else:
        subscores["escalation_rate"] = (0.5, 0.10)  # Low confidence when no activity

    # Blocked-resolution rate proxy
    leg_parks = signals.get("legitimate_parks", 0)
    avoid_parks = signals.get("avoidance_parks", 0)
    total_parks = leg_parks + avoid_parks
    if total_parks > 0:
        resolve_score = leg_parks / total_parks
        subscores["blocked_resolution"] = (resolve_score, 0.35)
        notes.append(f"{total_parks} park events ({leg_parks} structured, {avoid_parks} vague)")
    elif act > 0:
        # No parks = either fully autonomous or no blockers (good)
        subscores["blocked_resolution"] = (0.85, 0.20)
        notes.append("No park events in period")

    # response_requested rate (lower = more forward-firing)
    rr = signals.get("response_requested_count", 0)
    msg_sent = signals.get("message_sent_count", 0)
    if msg_sent > 0:
        rr_rate = rr / msg_sent
        # Per critic: don't expose this value in injected scorecard (Goodhart risk)
        rr_score = max(0.0, 1.0 - rr_rate * 3.0)  # >33% response_requested = 0.0
        subscores["response_requested"] = (rr_score, 0.30)
    else:
        subscores["response_requested"] = (0.7, 0.10)

    if not subscores:
        return 0.5, 0.0, notes
    total_weight = sum(w for _, w in subscores.values())
    score = sum(s * w for s, w in subscores.values()) / total_weight
    confidence = min(1.0, act / max(1, 3.0))
    return round(score, 3), round(confidence, 3), notes


# ---------------------------------------------------------------------------
# Report generation
# ---------------------------------------------------------------------------

def compute_composite(dim_scores: dict, weights: dict) -> float:
    """Weighted sum over populated dimensions (per TAAB8: not geometric mean)."""
    total_w = 0.0
    weighted_sum = 0.0
    for dim, weight in weights.items():
        if dim in dim_scores:
            score, confidence, _ = dim_scores[dim]
            if confidence > 0:
                effective_weight = weight * confidence
                weighted_sum += score * effective_weight
                total_w += effective_weight
    if total_w == 0:
        return 0.5
    return round(weighted_sum / total_w, 3)


def load_prior_scorecard(agent_id: str, period: str) -> float | None:
    """Try to load composite score from prior scorecard for trend calculation."""
    scorecard_dir = PROJECTS_DIR / agent_id / "scorecards"
    scorecard_path = scorecard_dir / f"{period}.md"
    if not scorecard_path.exists():
        return None
    try:
        text = scorecard_path.read_text()
        m = re.search(r"\*\*Composite:\*\*\s*([\d.]+)", text)
        if m:
            return float(m.group(1))
    except Exception:
        pass
    return None


def find_recent_artifacts(agent_id: str, since: datetime) -> tuple[list[str], list[str]]:
    """Find recent act trail entries — highest signal (positive) and lowest (negative)."""
    state = load_state(agent_id)
    trail = state.get("trail", [])
    trail_w = extract_trail_in_window(trail, since)

    positive = []
    negative = []

    for e in trail_w:
        if e.get("type") != "act":
            continue
        detail = e.get("detail", {})
        if not isinstance(detail, dict):
            continue
        action = str(detail.get("action", ""))
        result = str(detail.get("result", ""))
        ts = e.get("ts", "")

        combined = f"{action} {result}"
        combined_lower = combined.lower()

        # Negative signals: retractions, failures, corrective context
        neg_kws = ["retract", "reverted", "failed", "broken", "wrong", "error", "stalled"]
        if any(kw in combined_lower for kw in neg_kws):
            snippet = f"`{ts[:10]}` — {action[:120]}"
            if snippet not in negative:
                negative.append(snippet)
        elif action.strip():
            snippet = f"`{ts[:10]}` — {action[:120]}"
            if snippet not in positive:
                positive.append(snippet)

    return positive[-3:], negative[-3:]


def generate_scorecard(
    agent_id: str, period_str: str, period_days: int, since: datetime,
    charter: dict, is_reactive: bool
) -> str:
    """Generate the full scorecard markdown for one agent."""
    state = load_state(agent_id)
    trail = state.get("trail", [])
    wake_count = state.get("wake_count", 0)
    trail_w = extract_trail_in_window(trail, since)
    act_count_in_window = count_trail_by_type(trail_w, "act")

    messages = load_messages()
    mike_pending = load_mike_pending()

    # Cold-start check
    cold_start = wake_count < COLD_START_MIN_WAKES or act_count_in_window < COLD_START_MIN_ACTS
    confidence_tag = "insufficient" if cold_start else "normal"

    lines = []
    lines.append(f"# Agent Scorecard: {agent_id}")
    lines.append(f"")
    lines.append(f"**Period:** {period_str} (since {since.strftime('%Y-%m-%d')})")
    lines.append(f"**Generated:** {datetime.now(timezone.utc).strftime('%Y-%m-%dT%H:%M:%SZ')}")
    lines.append(f"**Archetype:** {'reactive-critic' if is_reactive else 'initiative-driver'}")
    lines.append(f"**Wake count (all-time):** {wake_count}")
    lines.append(f"**Trail entries in window:** {len(trail_w)}")
    lines.append(f"**Confidence:** {confidence_tag}")
    lines.append("")

    if cold_start:
        lines.append("> **Cold-start / insufficient data.** This agent has fewer than "
                     f"{COLD_START_MIN_WAKES} all-time wakes or fewer than "
                     f"{COLD_START_MIN_ACTS} actions in the window. "
                     "Scorecard shown for reference; agent is excluded from fleet rankings.")
        lines.append("")

    # --- QUALITY ---
    lines.append("## Quality")
    lines.append("")

    if is_reactive:
        q_signals = extract_quality_signals_reactive(agent_id, trail_w, since)
        q_score, q_conf, q_notes = score_quality_reactive(q_signals)

        lines.append("| Signal | Value |")
        lines.append("|--------|-------|")
        lines.append(f"| Reviews produced in period | {q_signals.get('review_count', 0)} |")
        lines.append(f"| HIGH findings generated | {q_signals.get('high_findings', 0)} |")
        lines.append(f"| MED findings generated | {q_signals.get('med_findings', 0)} |")
        lines.append(f"| LOW findings generated | {q_signals.get('low_findings', 0)} |")
    else:
        q_signals = extract_quality_signals_initiative(
            agent_id, trail_w, messages, mike_pending, since, period_days)
        q_score, q_conf, q_notes = score_quality_initiative(q_signals)

        lines.append("| Signal | Value |")
        lines.append("|--------|-------|")
        lines.append(f"| Critic review HIGH mentions | {q_signals.get('critic_high', 0)} |")
        lines.append(f"| Critic review MED mentions | {q_signals.get('critic_med', 0)} |")
        lines.append(f"| Corrective messages from Mike | {q_signals.get('corrective_msgs', 0)} |")
        lines.append(f"| Explicit retractions in trail | {q_signals.get('retraction_count', 0)} |")
        if period_days >= 30:
            lines.append(f"| Useful-artifact ratio | pending (30d+ window — not yet computed) |")

    lines.append("")
    for note in q_notes:
        lines.append(f"- {note}")
    lines.append("")
    lines.append(f"**Quality Score: {q_score}** (confidence: {'high' if q_conf > 0.6 else 'low' if q_conf < 0.3 else 'moderate'})")
    lines.append("")

    # --- VELOCITY ---
    lines.append("## Velocity")
    lines.append("")

    if is_reactive:
        v_signals = extract_velocity_signals_reactive(agent_id, trail_w, since)
        v_score, v_conf, v_notes = score_velocity_reactive(v_signals, period_days)

        lines.append("| Signal | Value |")
        lines.append("|--------|-------|")
        lines.append(f"| Reviews in period | {v_signals.get('reviews_in_period', 0)} |")
        lines.append(f"| Actions in period | {v_signals.get('act_count', 0)} |")
        lines.append("")
        lines.append("_Note: Reactive-critic velocity measures responsiveness, not initiative dispatch._")
    else:
        v_signals = extract_velocity_signals_initiative(agent_id, trail_w, since)
        v_score, v_conf, v_notes = score_velocity_initiative(v_signals, period_days)

        init_path = PROJECTS_DIR / agent_id / "initiative-tracking.md"
        init_status = "present" if init_path.exists() else "missing"
        lines.append("| Signal | Value |")
        lines.append("|--------|-------|")
        lines.append(f"| Actions (act entries) in period | {v_signals.get('act_count', 0)} |")
        lines.append(f"| initiative-tracking.md | {init_status} |")
        lines.append(f"| Updated in period | {v_signals.get('initiative_tracking_updated', False)} |")
        lines.append(f"| Inbox items processed | {v_signals.get('inbox_processed', 0)} |")
        lines.append(f"| Messages sent | {v_signals.get('messages_sent', 0)} |")

    lines.append("")
    for note in v_notes:
        lines.append(f"- {note}")
    lines.append("")
    lines.append(f"**Velocity Score: {v_score}** (confidence: {'high' if v_conf > 0.6 else 'low' if v_conf < 0.3 else 'moderate'})")
    lines.append("")

    # --- AUTONOMY ---
    lines.append("## Autonomy")
    lines.append("")

    a_signals = extract_autonomy_signals(agent_id, trail_w, messages, mike_pending, since)
    a_score, a_conf, a_notes = score_autonomy(a_signals, is_reactive)

    lines.append("| Signal | Value |")
    lines.append("|--------|-------|")
    lines.append(f"| Mike-pending items registered | {a_signals.get('mike_pending_count', 0)} |")
    lines.append(f"| Park events (structured) | {a_signals.get('legitimate_parks', 0)} |")
    lines.append(f"| Park events (vague) | {a_signals.get('avoidance_parks', 0)} |")
    lines.append(f"| response_requested messages | {a_signals.get('response_requested_count', 0)} |")
    lines.append(f"| Total messages sent | {a_signals.get('message_sent_count', 0)} |")
    lines.append("")
    for note in a_notes:
        lines.append(f"- {note}")
    lines.append("")
    lines.append(f"**Autonomy Score: {a_score}** (confidence: {'high' if a_conf > 0.6 else 'low' if a_conf < 0.3 else 'moderate'})")
    lines.append("")

    # --- COMPOSITE + TREND ---
    lines.append("## Composite + Trend")
    lines.append("")

    weights = WEIGHTS_REACTIVE if is_reactive else WEIGHTS_INITIATIVE
    dim_key_map = {
        "quality": (q_score, q_conf, q_notes),
        "velocity": (v_score, v_conf, v_notes),
        "autonomy": (a_score, a_conf, a_notes),
        "responsiveness": (v_score, v_conf, v_notes),
    }
    composite = compute_composite(dim_key_map, weights)

    prior = load_prior_scorecard(agent_id, period_str)
    trend_str = "N/A (first scorecard)"
    if prior is not None:
        delta = round(composite - prior, 3)
        if delta > 0.05:
            trend_str = f"+{delta} vs prior period (improving)"
        elif delta < -0.05:
            trend_str = f"{delta} vs prior period (**regression — review needed**)"
        else:
            trend_str = f"{delta:+.3f} vs prior period (stable)"

    lines.append(f"**Composite: {composite}**")
    if cold_start:
        lines.append(f"**Rank: excluded (cold-start / insufficient data)**")
    lines.append(f"**Trend: {trend_str}**")
    lines.append("")
    lines.append("| Dimension | Score | Weight | Contribution |")
    lines.append("|-----------|-------|--------|--------------|")
    for dim, weight in weights.items():
        score_val = dim_key_map.get(dim, (0, 0, []))[0]
        lines.append(f"| {dim.title()} | {score_val} | {int(weight*100)}% | {round(score_val * weight, 3)} |")
    lines.append("")

    # --- ARTIFACTS ---
    positives, negatives = find_recent_artifacts(agent_id, since)

    lines.append("## Last 3 highest-signal artifacts (positive)")
    lines.append("")
    if positives:
        for item in positives[-3:]:
            lines.append(f"- {item}")
    else:
        lines.append("- No positive artifact signals found in period")
    lines.append("")

    lines.append("## Last 3 lowest-signal artifacts (negative — what NOT to repeat)")
    lines.append("")
    if negatives:
        for item in negatives[-3:]:
            lines.append(f"- {item}")
    else:
        lines.append("- No negative artifact signals found in period")
    lines.append("")

    return "\n".join(lines)


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    parser = argparse.ArgumentParser(
        description="Agent performance scorecard — quality, velocity, autonomy"
    )
    grp = parser.add_mutually_exclusive_group(required=True)
    grp.add_argument("--agent", help="Agent ID to score")
    grp.add_argument("--all", action="store_true", help="Score all agents")
    parser.add_argument(
        "--period", choices=["7d", "14d", "30d"], default="14d",
        help="Scoring window (default: 14d per ergonomics-critic recommendation)"
    )
    parser.add_argument("--dry-run", action="store_true", help="Print scorecard; do not write files")
    parser.add_argument("--output", default=None, help="Output path override")
    args = parser.parse_args()

    period_days = parse_period(args.period)
    since = window_start(period_days)

    if args.all:
        agents = list_all_agents()
    else:
        agents = [args.agent]

    exit_code = 0
    for agent_id in agents:
        agent_dir = PROJECTS_DIR / agent_id
        if not agent_dir.exists():
            print(f"[WARN] Agent directory not found: {agent_dir}", file=sys.stderr)
            continue

        charter = load_charter(agent_id)
        is_reactive = is_reactive_critic(agent_id, charter)

        try:
            scorecard = generate_scorecard(
                agent_id, args.period, period_days, since, charter, is_reactive
            )
        except Exception as e:
            print(f"[ERROR] Failed to generate scorecard for {agent_id}: {e}", file=sys.stderr)
            exit_code = 1
            continue

        if args.dry_run:
            print(scorecard)
        else:
            if args.output:
                out_path = Path(args.output)
                if len(agents) > 1:
                    out_path = out_path / f"{agent_id}-{args.period}.md"
            else:
                scorecard_dir = PROJECTS_DIR / agent_id / "scorecards"
                scorecard_dir.mkdir(parents=True, exist_ok=True)
                out_path = scorecard_dir / f"{args.period}.md"

            # Atomic write
            tmp = out_path.with_suffix(".md.tmp")
            try:
                tmp.write_text(scorecard)
                tmp.rename(out_path)
                # Also write latest.md symlink-equivalent
                latest = out_path.parent / "latest.md"
                latest.write_text(scorecard)
                print(f"[OK] Scorecard written: {out_path}", file=sys.stderr)
            except Exception as e:
                print(f"[ERROR] Failed to write scorecard for {agent_id}: {e}", file=sys.stderr)
                exit_code = 1

    return exit_code


if __name__ == "__main__":
    sys.exit(main())
