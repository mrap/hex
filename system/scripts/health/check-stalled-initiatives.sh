#!/usr/bin/env bash
# check-stalled-initiatives.sh — detect stalled initiatives and auto-poke owners
#
# Critic revisions applied (HIGH severity, 2026-05-05):
#  - charter.yaml `initiatives:` discovery path REMOVED (zero agents have this field)
#  - Only initiative-tracking.md files with valid YAML frontmatter are monitored
#  - Required frontmatter fields: initiative_id, horizon, blocked_by, krs[]
#  - Skip initiatives missing frontmatter (no coverage = visible gap, not silent failure)
#  - Suppress stall alert when blocked_by is non-null; emit hex.initiative.blocked instead
#  - Anti-spam state in projects/cos/stall-monitor-state.json (file-locked)
#  - Directive: "confirm status" (not "drive or close" which implies ownership threat)
#
# Usage: check-stalled-initiatives.sh [--dry-run]
# Exit:  0 = no stalls, 1 = stalls found or script error
set -uo pipefail

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS_DIR="$(cd "$SELF_DIR/.." && pwd)"
HEX_DIR="$(cd "$SCRIPTS_DIR/../.." && pwd)"
HEX_ALERT="$SCRIPTS_DIR/hex-alert.sh"
HEX_EMIT="$HOME/.hex-events/hex_emit.py"
HEX_BIN="$HEX_DIR/.hex/bin/hex"
STATE_FILE="$HEX_DIR/projects/cos/stall-monitor-state.json"

DRY_RUN=0
for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=1 ;;
    *) echo "check-stalled-initiatives: unknown arg: $arg" >&2; exit 1 ;;
  esac
done

result="$(python3 - "$HEX_DIR" "$DRY_RUN" "$STATE_FILE" "$HEX_ALERT" "$HEX_EMIT" "$HEX_BIN" <<'PYEOF'
import sys, json, os, re, subprocess, fcntl
from datetime import datetime, timezone, timedelta
from pathlib import Path

hex_dir    = sys.argv[1]
dry_run    = sys.argv[2] == "1"
state_path = Path(sys.argv[3])
hex_alert  = sys.argv[4]
hex_emit   = sys.argv[5]
hex_bin    = sys.argv[6]

now = datetime.now(timezone.utc)
STALE_THRESHOLD  = timedelta(hours=48)
ANTISPAM_WINDOW  = timedelta(hours=24)
projects_dir     = Path(hex_dir) / "projects"

# ─── Frontmatter parser (stdlib-only, no PyYAML) ─────────────────────────────

def parse_frontmatter(text):
    """
    Extract YAML frontmatter block (between leading --- delimiters).
    Returns a dict of top-level key: value pairs, or None if no valid block.
    Handles: scalar strings/nulls, and one-level list (krs).
    """
    m = re.match(r'^---\r?\n(.*?)\r?\n---\r?\n', text, re.DOTALL)
    if not m:
        return None
    block = m.group(1)
    result = {}
    current_key = None
    current_list = None
    for line in block.splitlines():
        # Blank line — reset list context
        if not line.strip():
            if current_list is not None and current_key:
                result[current_key] = current_list
                current_list = None
                current_key = None
            continue
        # List item under current key
        if line.startswith('  - ') or line.startswith('  -\t'):
            if current_key:
                if current_list is None:
                    current_list = []
                raw = line.lstrip().lstrip('- ').strip()
                # inline dict: {id: x, metric: y, ...}
                item = {}
                for kv in re.finditer(r'(\w+):\s*([^,}]+)', raw):
                    item[kv.group(1)] = kv.group(2).strip().strip('"')
                current_list.append(item if item else raw)
            continue
        # Top-level key: value
        kv = re.match(r'^(\w[\w_-]*):\s*(.*)', line)
        if kv:
            # Flush previous list
            if current_list is not None and current_key:
                result[current_key] = current_list
                current_list = None
            current_key = kv.group(1)
            val = kv.group(2).strip()
            if val == '' or val == 'null' or val == '~':
                result[current_key] = None
            elif val.lower() == 'true':
                result[current_key] = True
            elif val.lower() == 'false':
                result[current_key] = False
            elif val.startswith('['):
                result[current_key] = None
            else:
                result[current_key] = val.strip('"').strip("'")
    if current_list is not None and current_key:
        result[current_key] = current_list
    return result

# ─── Progress signal helpers ──────────────────────────────────────────────────

def parse_iso(s):
    if not s:
        return None
    try:
        return datetime.fromisoformat(str(s).replace('Z', '+00:00'))
    except Exception:
        return None

def last_git_commit_time(project_dir):
    """Most recent git commit touching project_dir. Returns datetime or None."""
    try:
        r = subprocess.run(
            ['git', '-C', hex_dir, 'log', '-1', '--format=%cI', '--', str(project_dir)],
            capture_output=True, text=True, timeout=10
        )
        ts = r.stdout.strip()
        return parse_iso(ts) if ts else None
    except Exception:
        return None

def last_trail_mention(agent_id, initiative_id):
    """
    Scan the agent's state.json trail for the most recent entry that
    mentions initiative_id.  Returns datetime or None.
    """
    state_file = projects_dir / agent_id / 'state.json'
    if not state_file.exists():
        return None
    try:
        with open(state_file) as f:
            data = json.load(f)
        trail = data.get('trail', [])
        # Trail may be huge — scan backwards, stop after 200 entries
        for entry in reversed(trail[-200:]):
            content = json.dumps(entry)
            if initiative_id in content:
                ts = parse_iso(entry.get('ts') or entry.get('timestamp'))
                if ts:
                    return ts
    except Exception:
        pass
    return None

def last_progress_signal(tracking_file, agent_id, initiative_id, project_dir):
    """Return (datetime, signal_name) for the most recent progress signal."""
    signals = []

    # Signal 1: git commit touching this project directory
    t = last_git_commit_time(project_dir)
    if t:
        signals.append((t, 'git-commit'))

    # Signal 2: initiative-tracking.md file mtime
    try:
        mtime = os.path.getmtime(str(tracking_file))
        signals.append((datetime.fromtimestamp(mtime, tz=timezone.utc), 'file-mtime'))
    except Exception:
        pass

    # Signal 3: trail entry mentioning initiative_id
    t = last_trail_mention(agent_id, initiative_id)
    if t:
        signals.append((t, 'trail-mention'))

    if not signals:
        return None, 'none'
    return max(signals, key=lambda x: x[0])

# ─── Anti-spam state (file-locked) ───────────────────────────────────────────

def load_state():
    if not state_path.exists():
        return {"last_fired": {}}
    lock_path = state_path.with_suffix('.lock')
    lock_fd = open(lock_path, 'w')
    try:
        fcntl.flock(lock_fd, fcntl.LOCK_SH | fcntl.LOCK_NB)
        data = json.loads(state_path.read_text())
        fcntl.flock(lock_fd, fcntl.LOCK_UN)
        return data
    except (BlockingIOError, OSError):
        return {"last_fired": {}}
    except Exception:
        return {"last_fired": {}}
    finally:
        lock_fd.close()

def save_state(data):
    if dry_run:
        return
    lock_path = state_path.with_suffix('.lock')
    lock_fd = open(lock_path, 'w')
    try:
        fcntl.flock(lock_fd, fcntl.LOCK_EX)
        tmp = state_path.with_suffix('.tmp')
        tmp.write_text(json.dumps(data, indent=2))
        tmp.rename(state_path)
        fcntl.flock(lock_fd, fcntl.LOCK_UN)
    except Exception as e:
        print(f"  [warn] could not save stall state: {e}", file=sys.stderr)
    finally:
        lock_fd.close()

def recently_fired(state, initiative_id):
    ts_str = state.get("last_fired", {}).get(initiative_id)
    if not ts_str:
        return False
    ts = parse_iso(ts_str)
    if not ts:
        return False
    return (now - ts) < ANTISPAM_WINDOW

# ─── Alert / event / message helpers ─────────────────────────────────────────

def emit_event(event_name, payload):
    if not os.path.exists(hex_emit):
        return
    try:
        subprocess.run(
            [sys.executable, hex_emit, event_name, json.dumps(payload), 'hex:stall-monitor'],
            capture_output=True, text=True, timeout=10
        )
    except Exception:
        pass

def send_alert(severity, message):
    if not (os.path.exists(hex_alert) and os.access(hex_alert, os.X_OK)):
        print(f"  [alert-fallback] {severity} stall-monitor: {message}", file=sys.stderr)
        return
    try:
        subprocess.run(
            [hex_alert, severity, 'stall-monitor', message],
            capture_output=True, text=True, timeout=15
        )
    except Exception:
        pass

def send_agent_message(owner_id, initiative_id, horizon, last_signal_ts, last_signal_name):
    """Send a 'confirm status' message to the owner agent."""
    if not (os.path.exists(hex_bin) and os.access(hex_bin, os.X_OK)):
        print(f"  [msg-fallback] would message {owner_id} re {initiative_id}", file=sys.stderr)
        return
    age_h = int((now - last_signal_ts).total_seconds() / 3600) if last_signal_ts else '?'
    content = (
        f"Initiative {initiative_id} has had no detectable progress for ~{age_h}h "
        f"(last signal: {last_signal_name}). Horizon: {horizon}. "
        f"Please confirm status — is this active, blocked, or ready to close? "
        f"If blocked, update initiative-tracking.md frontmatter `blocked_by` field."
    )
    try:
        subprocess.run(
            [hex_bin, 'message', 'send',
             '--content', content,
             '--msg-type', 'agent',
             'hex:stall-monitor', owner_id],
            capture_output=True, text=True, timeout=15
        )
    except Exception:
        pass

# ─── Main scan ───────────────────────────────────────────────────────────────

state = load_state()
stalls_found = []
blocked_found = []
skipped_no_frontmatter = []
new_last_fired = dict(state.get("last_fired", {}))

tracking_files = sorted(projects_dir.glob("*/initiative-tracking.md"))

if not tracking_files:
    print("[check-stalled-initiatives] No initiative-tracking.md files found")
    sys.exit(0)

for tracking_file in tracking_files:
    project_dir  = tracking_file.parent
    agent_id     = project_dir.name

    try:
        text = tracking_file.read_text(encoding='utf-8', errors='replace')
    except Exception as e:
        print(f"  [warn] cannot read {tracking_file}: {e}", file=sys.stderr)
        continue

    fm = parse_frontmatter(text)
    if fm is None:
        skipped_no_frontmatter.append(agent_id)
        continue

    required = ['initiative_id', 'horizon']
    missing = [k for k in required if k not in fm]
    if missing:
        skipped_no_frontmatter.append(f"{agent_id} (missing: {', '.join(missing)})")
        continue

    initiative_id = fm['initiative_id']
    horizon       = fm.get('horizon', 'unknown')
    blocked_by    = fm.get('blocked_by')
    owner_id      = fm.get('owner', agent_id)

    if dry_run:
        print(f"  [dry-run] found initiative {initiative_id} in {agent_id} "
              f"(blocked_by={blocked_by!r}, horizon={horizon})")

    # Blocked suppression — emit blocked event, never stall
    if blocked_by:
        blocked_found.append(initiative_id)
        if dry_run:
            print(f"  [dry-run] {initiative_id}: blocked by {blocked_by!r} — would emit hex.initiative.blocked")
        else:
            emit_event('hex.initiative.blocked', {
                'initiative_id': initiative_id,
                'blocked_by': blocked_by,
                'owner': owner_id,
                'horizon': horizon,
                'ts': now.isoformat(),
            })
        continue

    # Anti-spam: skip if we fired within the window
    if recently_fired(state, initiative_id):
        if dry_run:
            print(f"  [dry-run] {initiative_id}: anti-spam suppressed (fired <24h ago)")
        continue

    # Compute last progress signal
    last_ts, signal_name = last_progress_signal(
        tracking_file, agent_id, initiative_id, project_dir
    )

    is_stale = (last_ts is None) or ((now - last_ts) > STALE_THRESHOLD)

    if dry_run:
        age_h = int((now - last_ts).total_seconds() / 3600) if last_ts else '∞'
        print(f"  [dry-run] {initiative_id}: last_signal={signal_name} "
              f"age={age_h}h stale={is_stale}")

    if not is_stale:
        continue

    # Stall detected
    stalls_found.append(initiative_id)
    age_desc = f"{int((now - last_ts).total_seconds()/3600)}h" if last_ts else ">48h"

    if dry_run:
        print(f"  [dry-run] STALL: {initiative_id} ({agent_id}) "
              f"— last signal: {signal_name} {age_desc} ago")
        print(f"  [dry-run] would alert + message {owner_id}")
    else:
        send_alert('WARN', f"initiative {initiative_id} stalled: no progress for {age_desc} (last: {signal_name})")
        emit_event('hex.initiative.stalled', {
            'initiative_id': initiative_id,
            'owner': owner_id,
            'last_signal': signal_name,
            'last_signal_ts': last_ts.isoformat() if last_ts else None,
            'age_hours': int((now - last_ts).total_seconds() / 3600) if last_ts else None,
            'horizon': horizon,
            'ts': now.isoformat(),
        })
        send_agent_message(owner_id, initiative_id, horizon, last_ts, signal_name)
        new_last_fired[initiative_id] = now.isoformat()

# Persist updated anti-spam state
if new_last_fired != state.get("last_fired", {}):
    save_state({"last_fired": new_last_fired})

# ─── Summary ─────────────────────────────────────────────────────────────────

total = len(tracking_files)
monitored = total - len(skipped_no_frontmatter)

if skipped_no_frontmatter:
    print(f"[check-stalled-initiatives] {len(skipped_no_frontmatter)}/{total} files skipped "
          f"(no valid frontmatter): {', '.join(skipped_no_frontmatter[:5])}"
          + (f" +{len(skipped_no_frontmatter)-5} more" if len(skipped_no_frontmatter) > 5 else ""))

if blocked_found:
    print(f"[check-stalled-initiatives] {len(blocked_found)} blocked (suppressed): "
          f"{', '.join(blocked_found)}")

if stalls_found:
    print(f"[check-stalled-initiatives] STALL: {len(stalls_found)} stalled initiatives: "
          f"{', '.join(stalls_found)}")
    sys.exit(1)

print(f"[check-stalled-initiatives] OK — monitored {monitored} initiatives, "
      f"0 stalls, {len(blocked_found)} blocked")
PYEOF
)"

exit_code=$?
echo "$result"
exit $exit_code
