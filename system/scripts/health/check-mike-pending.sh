#!/usr/bin/env bash
# check-mike-pending.sh — detect Mike-pending items, emit to hex.agent.needs-attention bus
# v2: tier:quiet/digest/direct-ping names, coalesced bus emission, DM user ID lookup
set -uo pipefail

HEX_DIR="${HEX_DIR:-$HOME/hex}"
JSONL_FILE="$HEX_DIR/projects/cos/mike-pending.jsonl"
STATE_FILE="$HEX_DIR/projects/cos/mike-pending-state.json"
AUDIT_LOG="$HEX_DIR/.hex/audit/actions.jsonl"
INTEGRATIONS_FILE="$HEX_DIR/.hex/data/integrations.json"
HEX_EMIT="$HOME/.hex-events/hex_emit.py"

DRY_RUN=false
[[ "${1:-}" == "--dry-run" ]] && DRY_RUN=true

if [[ ! -f "$JSONL_FILE" ]]; then
  echo "[check-mike-pending] ERROR: $JSONL_FILE not found" >&2
  exit 1
fi

python3 - \
  "$JSONL_FILE" "$STATE_FILE" "$AUDIT_LOG" \
  "$INTEGRATIONS_FILE" "$HEX_EMIT" "$DRY_RUN" \
  "$HEX_DIR" \
  <<'PYEOF'
import json
import os
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

jsonl_path       = Path(sys.argv[1])
state_path       = Path(sys.argv[2])
audit_log        = Path(sys.argv[3])
integrations_file = Path(sys.argv[4])
hex_emit         = sys.argv[5]
dry_run          = sys.argv[6].lower() == "true"
hex_dir          = sys.argv[7]

now = datetime.now(timezone.utc)

# ── Tier mapping ──────────────────────────────────────────────────────────────
# escalation_level → tier name (items may store integer or string tier field)
TIER_QUIET       = "tier:quiet"
TIER_DIGEST      = "tier:digest"
TIER_DIRECT_PING = "tier:direct-ping"

def level_to_tier(level: int) -> str:
    if level >= 2:
        return TIER_DIRECT_PING
    if level >= 1:
        return TIER_DIGEST
    return TIER_QUIET

def item_tier(item: dict) -> str:
    if "tier" in item:
        return item["tier"]
    return level_to_tier(item.get("escalation_level", 0))

# ── Helpers ───────────────────────────────────────────────────────────────────
def parse_ts(s):
    if not s:
        return None
    try:
        return datetime.fromisoformat(s.replace("Z", "+00:00"))
    except Exception:
        return None

def hours_since(ts_str):
    ts = parse_ts(ts_str)
    if ts is None:
        return float("inf")
    return (now - ts).total_seconds() / 3600

# ── Load items ────────────────────────────────────────────────────────────────
items = []
for line in jsonl_path.read_text().splitlines():
    line = line.strip()
    if line:
        items.append(json.loads(line))

# ── Load anti-spam state ───────────────────────────────────────────────────────
state = {
    "last_digest_ts": None,
    "last_direct_ping_ts": None,
    "terminal_taken": {},
}
if state_path.exists():
    try:
        state = json.loads(state_path.read_text())
    except Exception:
        pass

# ── Look up @mike Slack user ID for DM delivery ───────────────────────────────
mike_slack_user_id = None
if integrations_file.exists():
    try:
        cfg = json.loads(integrations_file.read_text())
        mike_slack_user_id = cfg.get("slack", {}).get("mike_user_id")
    except Exception:
        pass

dm_configured = bool(mike_slack_user_id)
if not dm_configured:
    print(
        "[check-mike-pending] WARN: .hex/data/integrations.json missing or "
        "slack.mike_user_id not set — tier:direct-ping will degrade to channel "
        "until configured. Filing Mike-pending blocker.",
        file=sys.stderr,
    )

# ── File DM-config blocker if missing ────────────────────────────────────────
DM_BLOCKER_ID = "MP-SLACK-DM-CONFIG"

def _blocker_already_filed() -> bool:
    for item in items:
        if item.get("id") == DM_BLOCKER_ID and not item.get("resolved"):
            return True
    return False

if not dm_configured and not _blocker_already_filed():
    blocker = {
        "id": DM_BLOCKER_ID,
        "subject": (
            "Slack user ID for @mike DM channel needed for tier:direct-ping escalation — "
            "set slack.mike_user_id in .hex/data/integrations.json"
        ),
        "deadline_ts": now.isoformat(),
        "blocking_initiative": None,
        "registered_at": now.isoformat(),
        "registered_by": "check-mike-pending",
        "escalation_level": 1,
        "tier": TIER_DIGEST,
        "last_escalation": None,
        "max_escalations": 3,
        "terminal_action": "archive",
        "deadline_refresh_interval": 1,
        "deadline_reasoning": "Config blocker: tier:direct-ping cannot DM until resolved.",
    }
    items.append(blocker)
    blocker_filed = True
    print(f"  [blocker] Filed {DM_BLOCKER_ID}: Slack DM user ID needed")
else:
    blocker_filed = False

# ── Process escalation levels ──────────────────────────────────────────────────
updated_items = []
escalated_ids = []
terminal_ids  = []

for item in items:
    deadline_ts    = parse_ts(item.get("deadline_ts"))
    max_esc        = item.get("max_escalations", 3)
    current_level  = item.get("escalation_level", 0)
    terminal_action = item.get("terminal_action", "archive")

    if state["terminal_taken"].get(item["id"]):
        updated_items.append(item)
        continue

    if deadline_ts and now > deadline_ts:
        if current_level < max_esc:
            current_level += 1
            item["escalation_level"] = current_level
            item["tier"] = level_to_tier(current_level)
            escalated_ids.append(item["id"])
            print(f"  [escalate] {item['id']} → {item['tier']} (level {current_level}/{max_esc})")
        elif current_level >= max_esc:
            terminal_ids.append((item["id"], terminal_action, item["subject"][:60]))
            state["terminal_taken"][item["id"]] = now.isoformat()
            print(f"  [terminal] {item['id']}: {terminal_action}")

    updated_items.append(item)

# ── Classify active items by tier ─────────────────────────────────────────────
active = [i for i in updated_items if not state["terminal_taken"].get(i["id"])]

digest_items      = [i for i in active if item_tier(i) == TIER_DIGEST]
direct_ping_items = [i for i in active if item_tier(i) == TIER_DIRECT_PING]

hours_since_digest = hours_since(state.get("last_digest_ts"))
hours_since_ping   = hours_since(state.get("last_direct_ping_ts"))

emit_digest = len(digest_items) > 0 and hours_since_digest >= 6
emit_ping   = len(direct_ping_items) > 0 and hours_since_ping >= 2

# ── Bus emission helper ────────────────────────────────────────────────────────
def emit_event(payload: dict) -> bool:
    if dry_run:
        print(f"  [dry-run] emit hex.agent.needs-attention: {json.dumps(payload)[:120]}")
        return True
    try:
        result = subprocess.run(
            [sys.executable, hex_emit,
             "hex.agent.needs-attention",
             json.dumps(payload),
             "hex:mike-pending"],
            timeout=10,
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            print(f"  [warn] hex_emit failed: {result.stderr.strip()[:120]}", file=sys.stderr)
            return False
        return True
    except Exception as e:
        print(f"  [warn] hex_emit exception: {e}", file=sys.stderr)
        return False

# ── Emit coalesced tier:digest event ──────────────────────────────────────────
if emit_digest:
    summary_parts = [f"{i['id']}: {i['subject'][:50]}" for i in digest_items]
    n = len(digest_items)
    payload = {
        "agent_id": "mike-pending-board",
        "reason_kind": "mike-pending",
        "source_mechanism": "mike-pending",
        "severity": "WARN",
        "message": f"{n} Mike-pending item(s) at tier:digest: {'; '.join(summary_parts[:3])}{'...' if n > 3 else ''}",
        "details_url": f"file://{hex_dir}/projects/cos/mike-pending.jsonl",
        "ttl_seconds": 86400,
        "tier": TIER_DIGEST,
        "item_ids": [i["id"] for i in digest_items],
        "item_count": n,
        "coalesced": True,
    }
    if emit_event(payload):
        if not dry_run:
            state["last_digest_ts"] = now.isoformat()
        print(f"  [tier:digest] Emitted bus event ({n} items)")
elif len(digest_items) > 0:
    remaining = max(0, 6 - hours_since_digest)
    print(f"  [tier:digest] Anti-spam: {hours_since_digest:.1f}h since last emit (next in {remaining:.1f}h)")
else:
    print("  [tier:digest] No items — skipping")

# ── Emit coalesced tier:direct-ping event ─────────────────────────────────────
if emit_ping:
    summary_parts = [f"{i['id']}: {i['subject'][:50]}" for i in direct_ping_items]
    n = len(direct_ping_items)
    payload = {
        "agent_id": "mike-pending-board",
        "reason_kind": "mike-pending",
        "source_mechanism": "mike-pending",
        "severity": "CRITICAL",
        "message": f"{n} Mike-pending item(s) at tier:direct-ping: {'; '.join(summary_parts[:3])}{'...' if n > 3 else ''}",
        "details_url": f"file://{hex_dir}/projects/cos/mike-pending.jsonl",
        "ttl_seconds": 86400,
        "tier": TIER_DIRECT_PING,
        "item_ids": [i["id"] for i in direct_ping_items],
        "item_count": n,
        "coalesced": True,
        # Bus consumer uses this field to route to DM vs channel
        "slack_dm_user_id": mike_slack_user_id,
        "dm_configured": dm_configured,
    }
    if emit_event(payload):
        if not dry_run:
            state["last_direct_ping_ts"] = now.isoformat()
        dm_note = f"DM to {mike_slack_user_id}" if dm_configured else "DEGRADED: channel (DM not configured)"
        print(f"  [tier:direct-ping] Emitted bus event ({n} items, {dm_note})")
elif len(direct_ping_items) > 0:
    remaining = max(0, 2 - hours_since_ping)
    print(f"  [tier:direct-ping] Anti-spam: {hours_since_ping:.1f}h since last emit (next in {remaining:.1f}h)")
else:
    print("  [tier:direct-ping] No items — skipping")

# ── Terminal action audit log ──────────────────────────────────────────────────
for tid, action, subj in terminal_ids:
    print(f"  [terminal-action] {tid}: {action} — {subj}")
    if not dry_run:
        try:
            audit_log.parent.mkdir(parents=True, exist_ok=True)
            with open(audit_log, "a") as f:
                f.write(json.dumps({
                    "ts": now.isoformat(),
                    "type": "mike-pending-terminal",
                    "id": tid,
                    "action": action,
                    "subject": subj,
                    "reason": f"max_escalations reached, took terminal_action={action}",
                }) + "\n")
        except Exception as e:
            print(f"  [warn] audit log write failed: {e}", file=sys.stderr)

# ── Atomic JSONL write-back ───────────────────────────────────────────────────
if not dry_run and (escalated_ids or terminal_ids or blocker_filed):
    tmp = jsonl_path.with_suffix(".jsonl.tmp")
    tmp.write_text("\n".join(json.dumps(i) for i in updated_items) + "\n")
    tmp.rename(jsonl_path)
    print(f"  [write] Updated mike-pending.jsonl ({len(escalated_ids)} escalated, {len(terminal_ids)} terminal)")
elif dry_run:
    print(f"  [dry-run] Would update {len(escalated_ids)} escalated, {len(terminal_ids)} terminal items")

# ── Atomic state write-back ───────────────────────────────────────────────────
if not dry_run:
    tmp_state = state_path.with_suffix(".json.tmp")
    tmp_state.write_text(json.dumps(state, indent=2))
    tmp_state.rename(state_path)

# ── Summary ───────────────────────────────────────────────────────────────────
all_active_tiers = {item_tier(i) for i in active}
print(
    f"\n[check-mike-pending] done — "
    f"{len(digest_items)} at tier:digest, "
    f"{len(direct_ping_items)} at tier:direct-ping, "
    f"{len(terminal_ids)} terminal actions | "
    f"dm_configured={dm_configured}"
)
sys.exit(0)
PYEOF
