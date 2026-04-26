#!/bin/bash
# meeting-prep.sh — Auto meeting prep from Google Calendar
# Queries upcoming meetings via claude -p (with Google Calendar MCP),
# generates prep docs, notifies via Telegram.
#
# Usage:
#   bash meeting-prep.sh               # Normal run (check next 2 hours)
#   bash meeting-prep.sh --cron-install # Install cron entry
#   bash meeting-prep.sh --test         # Run self-test with mock data
#
# The script checks work hours internally (9am-6pm in configured timezone).
# Cron: */30 * * * * cd ~/hex && bash .hex/scripts/meeting-prep.sh
set -uo pipefail

# --- Resolve agent directory ---
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AGENT_DIR=""
candidate="$SCRIPT_DIR"
while [ "$candidate" != "/" ]; do
  if [ -f "$candidate/CLAUDE.md" ]; then
    AGENT_DIR="$candidate"
    break
  fi
  candidate="$(dirname "$candidate")"
done

if [ -z "${AGENT_DIR:-}" ]; then
  echo "Error: Could not find CLAUDE.md." >&2
  exit 1
fi

# --- LLM CLI abstraction ---
source "$AGENT_DIR/.hex/scripts/llm-cli.sh"

# --- Config ---
TZ_FILE="$AGENT_DIR/.hex/timezone"
PREP_DIR="$AGENT_DIR/raw/meeting-prep"
DONE_FILE="$PREP_DIR/.prepped-today.json"
LOG_FILE="$PREP_DIR/.meeting-prep.log"
TODAY="$(date +%Y-%m-%d)"
TG_CONFIG="${HOME}/.config/hex/telegram.yaml"
NTFY_SCRIPT="$AGENT_DIR/.hex/scripts/hex-notify.sh"

TZ_NAME="America/New_York"
if [ -f "$TZ_FILE" ]; then
    TZ_NAME=$(tr -d '[:space:]' < "$TZ_FILE")
fi
export TZ="$TZ_NAME"

mkdir -p "$PREP_DIR"

log() { echo "[meeting-prep $(date +%H:%M:%S)] $*" >> "$LOG_FILE" 2>/dev/null; }

log "=== Starting ==="

# --- Cron install helper ---
if [[ "${1:-}" == "--cron-install" ]]; then
    CRON_LINE="*/30 * * * * cd ${AGENT_DIR} && bash .hex/scripts/meeting-prep.sh >> /dev/null 2>&1"
    if crontab -l 2>/dev/null | grep -qF "meeting-prep.sh"; then
        echo "Cron entry already exists."
    else
        (crontab -l 2>/dev/null; echo "$CRON_LINE") | crontab -
        echo "Installed: $CRON_LINE"
    fi
    exit 0
fi

# --- Self-test mode ---
if [[ "${1:-}" == "--test" ]]; then
    TEST_DIR=$(mktemp -d)
    trap 'rm -rf "$TEST_DIR"' EXIT

    TEST_PREP_DIR="$TEST_DIR/prep"
    TEST_DONE_FILE="$TEST_PREP_DIR/.prepped-today.json"
    TEST_TODAY="2026-01-15"
    TEST_LOG_FILE="$TEST_DIR/test.log"

    mkdir -p "$TEST_PREP_DIR"

    # Mock response with two meetings (one normal, one to test project matching)
    MOCK_RESPONSE='===MEETING===
TITLE: Weekly Sync with Alice
TIME_START: 2026-01-15T10:00:00
TIME_END: 2026-01-15T10:30:00
MINUTES_UNTIL: 45
EVENT_ID: evt_test_001
ATTENDEES: Alice Smith, Bob Jones
PROJECT: none
===PREP===
# Meeting Prep: Weekly Sync with Alice

**Time:** 10:00 AM - 10:30 AM ET
**With:** Alice Smith, Bob Jones

## Context
Weekly sync to discuss project progress.

## Talking Points
- Review sprint status
- Discuss blockers
- Plan next steps

## Open Threads
No open threads found.

## Notes
_Space for notes during the meeting_
===END===

===MEETING===
TITLE: Design Review
TIME_START: 2026-01-15T11:00:00
TIME_END: 2026-01-15T12:00:00
MINUTES_UNTIL: 105
EVENT_ID: evt_test_002
ATTENDEES: Carol Lee
PROJECT: none
===PREP===
# Meeting Prep: Design Review

**Time:** 11:00 AM - 12:00 PM ET
**With:** Carol Lee

## Context
Review new UI designs.

## Talking Points
- Review mockups
- Discuss user flow

## Open Threads
No open threads found.

## Notes
_Space for notes during the meeting_
===END==='

    # Write mock response to the response tmp file
    RESPONSE_TMP="$TEST_PREP_DIR/.response.tmp"
    printf '%s' "$MOCK_RESPONSE" > "$RESPONSE_TMP"

    # Run the Python parser with test env vars
    export AGENT_DIR="$TEST_DIR"
    export TODAY="$TEST_TODAY"
    export PREP_DIR="$TEST_PREP_DIR"
    export DONE_FILE="$TEST_DONE_FILE"
    export TG_CONFIG="/nonexistent/telegram.yaml"
    export NTFY_SCRIPT="/nonexistent/hex-notify.sh"
    export LOG_FILE="$TEST_LOG_FILE"

    PASS=0
    FAIL=0
    report() {
        local label="$1" ok="$2"
        if [ "$ok" = "1" ]; then
            echo "  PASS: $label"
            PASS=$((PASS + 1))
        else
            echo "  FAIL: $label"
            FAIL=$((FAIL + 1))
        fi
    }

    echo "=== meeting-prep.sh --test ==="

    # Test 1: Run the Python parser (inline heredoc from the main script)
    python3 << 'PYEOF'
import json
import os
import re
import subprocess
import sys
import urllib.error
import urllib.request
from datetime import datetime
from pathlib import Path

AGENT_DIR = os.environ["AGENT_DIR"]
TODAY = os.environ["TODAY"]
PREP_DIR = os.environ["PREP_DIR"]
DONE_FILE = os.environ["DONE_FILE"]
TG_CONFIG = os.environ["TG_CONFIG"]
NTFY_SCRIPT = os.environ["NTFY_SCRIPT"]
LOG_FILE = os.environ["LOG_FILE"]


def log(msg):
    try:
        with open(LOG_FILE, "a") as f:
            f.write(f"[meeting-prep {datetime.now().strftime('%H:%M:%S')}] {msg}\n")
    except Exception:
        pass


# --- Read response ---
response_file = os.path.join(PREP_DIR, ".response.tmp")
try:
    response = Path(response_file).read_text()
except OSError:
    log("Could not read response file")
    sys.exit(0)

# --- Parse meeting blocks ---
meetings = []
blocks = re.split(r"===MEETING===", response)
for block in blocks[1:]:
    end_match = re.search(r"===END===", block)
    if not end_match:
        continue
    block = block[: end_match.start()]

    header_match = re.search(r"===PREP===", block)
    if not header_match:
        continue
    header = block[: header_match.start()]
    content = block[header_match.end() :]

    meeting = {}
    for line in header.strip().split("\n"):
        if ":" in line:
            key, _, val = line.partition(":")
            meeting[key.strip()] = val.strip()
    meeting["content"] = content.strip()
    meetings.append(meeting)

if not meetings:
    log("No meetings parsed from response")
    sys.exit(0)

log(f"Parsed {len(meetings)} meeting(s)")

# --- Load done file ---
done_data = {"date": TODAY, "event_ids": [], "preps": {}}
if os.path.exists(DONE_FILE):
    try:
        with open(DONE_FILE) as f:
            done_data = json.load(f)
        if "preps" not in done_data:
            done_data["preps"] = {}
    except (json.JSONDecodeError, OSError):
        done_data = {"date": TODAY, "event_ids": [], "preps": {}}

# --- Process each meeting ---
notifications = []
for m in meetings:
    event_id = m.get("EVENT_ID", "")
    title = m.get("TITLE", "Unknown Meeting")
    minutes_until = m.get("MINUTES_UNTIL", "?")
    project = m.get("PROJECT", "none").strip()
    content = m.get("content", "")

    if event_id and event_id in done_data.get("event_ids", []):
        log(f"  Skip (already prepped): {title}")
        continue

    # Determine output directory: project dir or raw/meeting-prep
    out_dir = PREP_DIR
    if project and project != "none":
        proj_dir = os.path.join(AGENT_DIR, "projects", project)
        if os.path.isdir(proj_dir):
            proj_meetings = os.path.join(proj_dir, "meetings")
            os.makedirs(proj_meetings, exist_ok=True)
            out_dir = proj_meetings

    # Generate filename
    safe_title = re.sub(r"[^a-zA-Z0-9-]", "-", title.lower())[:40].strip("-")
    safe_title = re.sub(r"-+", "-", safe_title)
    filename = f"meeting-prep-{TODAY}-{safe_title}.md"
    filepath = os.path.join(out_dir, filename)

    if os.path.exists(filepath):
        log(f"  Skip (file exists): {filepath}")
        continue

    # Write atomically
    tmp_path = filepath + ".tmp"
    with open(tmp_path, "w") as f:
        f.write(content + "\n")
    os.rename(tmp_path, filepath)
    log(f"  Saved: {filepath}")

    # Track as prepped
    if event_id:
        done_data["event_ids"].append(event_id)
        done_data["preps"][event_id] = filepath

    notifications.append(
        {"title": title, "minutes": minutes_until, "path": filepath, "event_id": event_id}
    )

# --- Save done file atomically ---
done_data["date"] = TODAY
tmp_done = DONE_FILE + ".tmp"
with open(tmp_done, "w") as f:
    json.dump(done_data, f, indent=2)
os.rename(tmp_done, DONE_FILE)

# Skip Telegram notifications in test mode (config paths don't exist)
log(f"=== Done. {len(notifications)} prep(s) generated. ===")
PYEOF

    PARSER_EXIT=$?
    report "Python parser exits cleanly" "$([ $PARSER_EXIT -eq 0 ] && echo 1 || echo 0)"

    # Test 2: Check that prep files were created
    FILE1="$TEST_PREP_DIR/meeting-prep-${TEST_TODAY}-weekly-sync-with-alice.md"
    FILE2="$TEST_PREP_DIR/meeting-prep-${TEST_TODAY}-design-review.md"
    report "Prep file 1 created (weekly-sync)" "$([ -f "$FILE1" ] && echo 1 || echo 0)"
    report "Prep file 2 created (design-review)" "$([ -f "$FILE2" ] && echo 1 || echo 0)"

    # Test 3: Check file content
    if [ -f "$FILE1" ]; then
        report "Prep file 1 has meeting title" "$(grep -q 'Weekly Sync with Alice' "$FILE1" && echo 1 || echo 0)"
        report "Prep file 1 has talking points" "$(grep -q 'Talking Points' "$FILE1" && echo 1 || echo 0)"
    else
        report "Prep file 1 has meeting title" "0"
        report "Prep file 1 has talking points" "0"
    fi

    # Test 4: Check done file tracking
    report "Done file created" "$([ -f "$TEST_DONE_FILE" ] && echo 1 || echo 0)"
    if [ -f "$TEST_DONE_FILE" ]; then
        report "Done file tracks evt_test_001" "$(grep -q 'evt_test_001' "$TEST_DONE_FILE" && echo 1 || echo 0)"
        report "Done file tracks evt_test_002" "$(grep -q 'evt_test_002' "$TEST_DONE_FILE" && echo 1 || echo 0)"
        report "Done file has correct date" "$(grep -q "$TEST_TODAY" "$TEST_DONE_FILE" && echo 1 || echo 0)"
        report "Done file has prep path for evt_test_001" "$(python3 -c "
import json, sys
d = json.load(open(sys.argv[1]))
ok = 'evt_test_001' in d.get('preps', {})
print('1' if ok else '0')
" "$TEST_DONE_FILE" 2>/dev/null)"
    else
        report "Done file tracks evt_test_001" "0"
        report "Done file tracks evt_test_002" "0"
        report "Done file has correct date" "0"
        report "Done file has prep path for evt_test_001" "0"
    fi

    # Test 5: Re-run parser — should skip already-prepped events
    printf '%s' "$MOCK_RESPONSE" > "$RESPONSE_TMP"
    python3 << 'PYEOF2'
import json, os, re, sys
from pathlib import Path
from datetime import datetime

PREP_DIR = os.environ["PREP_DIR"]
DONE_FILE = os.environ["DONE_FILE"]
TODAY = os.environ["TODAY"]
LOG_FILE = os.environ["LOG_FILE"]

def log(msg):
    try:
        with open(LOG_FILE, "a") as f:
            f.write(f"[test-rerun {datetime.now().strftime('%H:%M:%S')}] {msg}\n")
    except Exception:
        pass

response = Path(os.path.join(PREP_DIR, ".response.tmp")).read_text()
meetings = []
blocks = re.split(r"===MEETING===", response)
for block in blocks[1:]:
    end_match = re.search(r"===END===", block)
    if not end_match: continue
    block = block[:end_match.start()]
    header_match = re.search(r"===PREP===", block)
    if not header_match: continue
    header = block[:header_match.start()]
    content = block[header_match.end():]
    meeting = {}
    for line in header.strip().split("\n"):
        if ":" in line:
            key, _, val = line.partition(":")
            meeting[key.strip()] = val.strip()
    meeting["content"] = content.strip()
    meetings.append(meeting)

done_data = {"date": TODAY, "event_ids": [], "preps": {}}
if os.path.exists(DONE_FILE):
    try:
        with open(DONE_FILE) as f:
            done_data = json.load(f)
        if "preps" not in done_data:
            done_data["preps"] = {}
    except (json.JSONDecodeError, OSError):
        done_data = {"date": TODAY, "event_ids": [], "preps": {}}

new_count = 0
for m in meetings:
    event_id = m.get("EVENT_ID", "")
    if event_id and event_id in done_data.get("event_ids", []):
        log(f"  Skip (already prepped): {m.get('TITLE','')}")
        continue
    new_count += 1

# Write result for the test harness
with open(os.path.join(PREP_DIR, ".rerun-count"), "w") as f:
    f.write(str(new_count))
PYEOF2

    RERUN_COUNT=$(cat "$TEST_PREP_DIR/.rerun-count" 2>/dev/null || echo "-1")
    report "Re-run skips already-prepped events (new=0)" "$([ "$RERUN_COUNT" = "0" ] && echo 1 || echo 0)"
    rm -f "$RESPONSE_TMP" "$TEST_PREP_DIR/.rerun-count"

    # Test 6: No .tmp files left behind
    TMP_COUNT=$(find "$TEST_DIR" -name "*.tmp" 2>/dev/null | wc -l)
    report "No .tmp files left behind" "$([ "$TMP_COUNT" -eq 0 ] && echo 1 || echo 0)"

    # Test 7: Log file exists and has entries
    report "Log file has entries" "$([ -s "$TEST_LOG_FILE" ] && echo 1 || echo 0)"

    echo ""
    echo "Results: $PASS passed, $FAIL failed"
    if [ "$FAIL" -gt 0 ]; then
        exit 1
    fi
    exit 0
fi

# --- Work hours check (9-18 in configured TZ) ---
HOUR=$(date +%-H)
if [ "$HOUR" -lt 9 ] || [ "$HOUR" -ge 18 ]; then
    log "Outside work hours ($HOUR), skip."
    exit 0
fi

# --- LLM CLI check ---
if [ -z "$LLM_CLI" ]; then
    log "No LLM CLI found (need claude or codex), skip."
    exit 0
fi

# --- Reset done file on new day ---
if [ -f "$DONE_FILE" ]; then
    DONE_DATE=$(python3 -c "
import json, sys
try:
    d = json.load(open(sys.argv[1]))
    print(d.get('date',''))
except: print('')
" "$DONE_FILE" 2>/dev/null) || DONE_DATE=""
    if [ "$DONE_DATE" != "$TODAY" ]; then
        rm -f "$DONE_FILE"
    fi
fi

# --- Gather context ---
PEOPLE_CTX=""
if [ -d "$AGENT_DIR/people" ]; then
    for p in "$AGENT_DIR"/people/*/profile.md; do
        [ -f "$p" ] || continue
        NAME=$(basename "$(dirname "$p")")
        PEOPLE_CTX="${PEOPLE_CTX}
--- ${NAME} ---
$(head -30 "$p")
"
    done
fi

TODO_CTX=""
[ -f "$AGENT_DIR/todo.md" ] && TODO_CTX=$(head -100 "$AGENT_DIR/todo.md")

# Already-prepped event IDs
ALREADY=""
if [ -f "$DONE_FILE" ]; then
    ALREADY=$(python3 -c "
import json, sys
try:
    d = json.load(open(sys.argv[1]))
    print(','.join(d.get('event_ids',[])))
except: print('')
" "$DONE_FILE" 2>/dev/null) || true
fi

# Project names for matching
PROJECTS=$(ls -1 "$AGENT_DIR/projects/" 2>/dev/null | tr '\n' ', ')

# Time range
NOW_ISO=$(date +%Y-%m-%dT%H:%M:%S)
TWO_H=$(python3 -c "from datetime import datetime,timedelta;print((datetime.now()+timedelta(hours=2)).strftime('%Y-%m-%dT%H:%M:%S'))")

# Truncate large context
[ ${#PEOPLE_CTX} -gt 5000 ] && PEOPLE_CTX="${PEOPLE_CTX:0:5000}...(truncated)"
[ ${#TODO_CTX} -gt 3000 ] && TODO_CTX="${TODO_CTX:0:3000}...(truncated)"

# --- Build prompt (write to temp file to avoid arg-too-long) ---
PROMPT_TMP="$PREP_DIR/.prompt.tmp"
cat > "$PROMPT_TMP" << PROMPTEOF
You are an executive assistant preparing meeting prep docs. Current time: ${NOW_ISO} (${TZ_NAME}).

STEP 1: Query Google Calendar for events between now and 2 hours from now.
Use gcal_list_events with:
  timeMin="${NOW_ISO}"
  timeMax="${TWO_H}"
  timeZone="${TZ_NAME}"
  condenseEventDetails=false (we need the attendee list)

STEP 2: Filter events:
- INCLUDE only events with 2 or more attendees
- SKIP events with these words in title (case-insensitive): "Break", "Focus", "Lunch", "OOO", "Block", "Hold"
- SKIP events with more than 8 attendees (all-hands / large meetings)
- SKIP events where myResponseStatus is "declined"
- SKIP already-prepped event IDs: ${ALREADY}

STEP 3: For each qualifying meeting, generate a prep doc using context below.

KNOWN PEOPLE:
${PEOPLE_CTX:-No people profiles available.}

CURRENT PRIORITIES/TODOS:
${TODO_CTX:-No todos available.}

PROJECTS: ${PROJECTS}

STEP 4: Output in this EXACT format (one block per meeting):

===MEETING===
TITLE: <meeting title>
TIME_START: <ISO timestamp>
TIME_END: <ISO timestamp>
MINUTES_UNTIL: <integer minutes until meeting>
EVENT_ID: <calendar event id>
ATTENDEES: <comma-separated names>
PROJECT: <matching project name from PROJECTS list, or "none">
===PREP===
# Meeting Prep: <title>

**Time:** <formatted time range>
**With:** <attendee names and roles if known>

## Context
<Brief background on this meeting and the people involved>

## Talking Points
- <point 1>
- <point 2>
- <point 3>

## Open Threads
<Relevant items from todos or past interactions. If none, say "No open threads found.">

## Notes
_Space for notes during the meeting_
===END===

If there are NO qualifying meetings, output exactly:
===NO_MEETINGS===

IMPORTANT: Output ONLY the structured format above. No preamble or extra text.
PROMPTEOF

log "Calling $LLM_CLI (prompt: $(wc -c < "$PROMPT_TMP" | tr -d ' ') bytes)..."

RESPONSE=""
RESPONSE=$(
    # Strip CLAUDE* env vars to avoid session conflicts
    for var in $(env | grep '^CLAUDE' | cut -d= -f1); do
        unset "$var"
    done
    cd "$AGENT_DIR"
    timeout 90 llm_exec "$(cat "$PROMPT_TMP")" 2>/dev/null
) || {
    EXIT_CODE=$?
    log "$LLM_CLI failed (exit: $EXIT_CODE)"
    rm -f "$PROMPT_TMP"
    exit 0
}
rm -f "$PROMPT_TMP"

if [ -z "$RESPONSE" ]; then
    log "Empty response, skip."
    exit 0
fi

log "Got response (${#RESPONSE} chars)"

# --- Check for no meetings ---
if echo "$RESPONSE" | grep -q "===NO_MEETINGS==="; then
    log "No qualifying meetings in next 2 hours."
    exit 0
fi

# --- Parse, save, and notify using Python ---
RESPONSE_TMP="$PREP_DIR/.response.tmp"
printf '%s' "$RESPONSE" > "$RESPONSE_TMP"

export AGENT_DIR TODAY PREP_DIR DONE_FILE TG_CONFIG NTFY_SCRIPT LOG_FILE

python3 << 'PYEOF'
import json
import os
import re
import subprocess
import sys
import urllib.error
import urllib.request
from datetime import datetime
from pathlib import Path

AGENT_DIR = os.environ["AGENT_DIR"]
TODAY = os.environ["TODAY"]
PREP_DIR = os.environ["PREP_DIR"]
DONE_FILE = os.environ["DONE_FILE"]
TG_CONFIG = os.environ["TG_CONFIG"]
NTFY_SCRIPT = os.environ["NTFY_SCRIPT"]
LOG_FILE = os.environ["LOG_FILE"]


def log(msg):
    try:
        with open(LOG_FILE, "a") as f:
            f.write(f"[meeting-prep {datetime.now().strftime('%H:%M:%S')}] {msg}\n")
    except Exception:
        pass


# --- Read response ---
response_file = os.path.join(PREP_DIR, ".response.tmp")
try:
    response = Path(response_file).read_text()
except OSError:
    log("Could not read response file")
    sys.exit(0)

# --- Parse meeting blocks ---
meetings = []
blocks = re.split(r"===MEETING===", response)
for block in blocks[1:]:
    end_match = re.search(r"===END===", block)
    if not end_match:
        continue
    block = block[: end_match.start()]

    header_match = re.search(r"===PREP===", block)
    if not header_match:
        continue
    header = block[: header_match.start()]
    content = block[header_match.end() :]

    meeting = {}
    for line in header.strip().split("\n"):
        if ":" in line:
            key, _, val = line.partition(":")
            meeting[key.strip()] = val.strip()
    meeting["content"] = content.strip()
    meetings.append(meeting)

if not meetings:
    log("No meetings parsed from response")
    sys.exit(0)

log(f"Parsed {len(meetings)} meeting(s)")

# --- Load done file ---
done_data = {"date": TODAY, "event_ids": [], "preps": {}}
if os.path.exists(DONE_FILE):
    try:
        with open(DONE_FILE) as f:
            done_data = json.load(f)
        if "preps" not in done_data:
            done_data["preps"] = {}
    except (json.JSONDecodeError, OSError):
        done_data = {"date": TODAY, "event_ids": [], "preps": {}}

# --- Process each meeting ---
notifications = []
for m in meetings:
    event_id = m.get("EVENT_ID", "")
    title = m.get("TITLE", "Unknown Meeting")
    minutes_until = m.get("MINUTES_UNTIL", "?")
    project = m.get("PROJECT", "none").strip()
    content = m.get("content", "")

    if event_id and event_id in done_data.get("event_ids", []):
        log(f"  Skip (already prepped): {title}")
        continue

    # Determine output directory: project dir or raw/meeting-prep
    out_dir = PREP_DIR
    if project and project != "none":
        proj_dir = os.path.join(AGENT_DIR, "projects", project)
        if os.path.isdir(proj_dir):
            proj_meetings = os.path.join(proj_dir, "meetings")
            os.makedirs(proj_meetings, exist_ok=True)
            out_dir = proj_meetings

    # Generate filename
    safe_title = re.sub(r"[^a-zA-Z0-9-]", "-", title.lower())[:40].strip("-")
    safe_title = re.sub(r"-+", "-", safe_title)
    filename = f"meeting-prep-{TODAY}-{safe_title}.md"
    filepath = os.path.join(out_dir, filename)

    if os.path.exists(filepath):
        log(f"  Skip (file exists): {filepath}")
        continue

    # Write atomically
    tmp_path = filepath + ".tmp"
    with open(tmp_path, "w") as f:
        f.write(content + "\n")
    os.rename(tmp_path, filepath)
    log(f"  Saved: {filepath}")

    # Track as prepped
    if event_id:
        done_data["event_ids"].append(event_id)
        done_data["preps"][event_id] = filepath

    notifications.append(
        {"title": title, "minutes": minutes_until, "path": filepath, "event_id": event_id}
    )

# --- Save done file atomically ---
done_data["date"] = TODAY
tmp_done = DONE_FILE + ".tmp"
with open(tmp_done, "w") as f:
    json.dump(done_data, f, indent=2)
os.rename(tmp_done, DONE_FILE)


# --- Telegram notification ---
def read_tg_config():
    token = os.environ.get("HEX_BOT_TOKEN", "")
    chat_id = ""
    if os.path.exists(TG_CONFIG):
        with open(TG_CONFIG) as f:
            for line in f:
                line = line.strip()
                if line.startswith("chat_id:"):
                    chat_id = line.split(":", 1)[1].strip().strip("\"'")
                if not token and line.startswith("bot_token:"):
                    val = line.split(":", 1)[1].strip().strip("\"'")
                    env_match = re.match(r"^\$\{(\w+)\}$", val)
                    if env_match:
                        token = os.environ.get(env_match.group(1), "")
                    else:
                        token = val
    return token, chat_id


def escape_md(text):
    special = set("_*[]()~`>#+-=|{}.!\\")
    return "".join("\\" + c if c in special else c for c in str(text))


def send_telegram(token, chat_id, title, minutes, event_id):
    msg = (
        f"\U0001f4cb *Meeting Prep Ready*\n\n"
        f"*{escape_md(title)}*\n"
        f"In {escape_md(minutes)} minutes\n\n"
        f"Prep doc saved\\."
    )
    # Callback data: mprep_<event_id> (must be <=64 bytes)
    cb_data = f"mprep_{event_id}"[:64] if event_id else "mprep_none"
    keyboard = {
        "inline_keyboard": [
            [{"text": "\U0001f4c4 View Prep", "callback_data": cb_data}]
        ]
    }
    payload = json.dumps(
        {
            "chat_id": chat_id,
            "text": msg,
            "parse_mode": "MarkdownV2",
            "reply_markup": keyboard,
        }
    )
    url = f"https://api.telegram.org/bot{token}/sendMessage"
    req = urllib.request.Request(
        url,
        data=payload.encode("utf-8"),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=15) as resp:
            result = json.loads(resp.read().decode("utf-8"))
            return result.get("ok", False)
    except Exception as e:
        log(f"Telegram send failed: {e}")
        return False


# Send notifications
token, chat_id = read_tg_config()
for n in notifications:
    if token and chat_id:
        tg_ok = send_telegram(token, chat_id, n["title"], n["minutes"], n["event_id"])
        if tg_ok:
            log(f"  Telegram sent: {n['title']}")
        else:
            log(f"  Telegram failed: {n['title']}")
    else:
        log(f"  Skipped notification (no Telegram config): {n['title']}")

log(f"=== Done. {len(notifications)} prep(s) generated. ===")
PYEOF

rm -f "$RESPONSE_TMP"
