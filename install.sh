#!/usr/bin/env bash
# sync-safe
set -euo pipefail

# hex install — Creates a hex instance on the user's machine.
# Usage: bash install.sh [target_dir]
#
# hex is an all-or-nothing package. BOI (parallel workers) is integral —
# there are no flags to skip it.
#
# The repo is the installer, not the workspace. This script creates a
# separate instance directory. The repo is disposable after install.

VERSION=$(cat "$(dirname "${BASH_SOURCE[0]}")/system/version.txt" 2>/dev/null || echo "0.1.0")
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TARGET_DIR=""

for arg in "$@"; do
    case "$arg" in
        --help|-h)   echo "Usage: bash install.sh [target_dir]"; exit 0 ;;
        -*)          echo "Unknown flag: $arg"; exit 1 ;;
        *)           TARGET_DIR="$arg" ;;
    esac
done

TARGET_DIR="${TARGET_DIR:-$HOME/hex}"
TARGET_DIR="${TARGET_DIR/#\~/$HOME}"

# macOS signed-install integration is deliberately a thin caller boundary.
# The common transaction owns mode detection, policy checks, staging,
# publication, compatibility paths, and state. This script must not reproduce
# any of those rules or fall back after a managed transaction fails.
MACOS_APP_INSTALLER="$SCRIPT_DIR/system/scripts/macos-app-install.py"
MACOS_APP_MODE="legacy-raw"
MACOS_APP_MANAGED=false
MACOS_APP_POLICY_AVAILABLE=false
MACOS_APP_SOURCE_REVISION=""
MACOS_APP_SERVICE_RECOVERY_PENDING=false

_macos_app_enabled() {
    [ "$(uname -s)" = "Darwin" ]
}

_macos_app_json() {
    local command=$1 product=$2 root=$3
    shift 3
    /usr/bin/python3 -I -B "$MACOS_APP_INSTALLER" "$command" "$product" --root "$root" "$@"
}

_macos_app_mode() {
    local product=$1 root=$2 payload
    payload="$(_macos_app_json mode "$product" "$root")" || return 1
    /usr/bin/python3 -I -B -c '
import json,sys
value=json.loads(sys.stdin.read())
product=sys.argv[1]
if not isinstance(value, dict) or type(value.get("schema_version")) is not int or value.get("schema_version") != 1 or type(value.get("product")) is not str or value.get("product") != product:
    raise SystemExit("invalid macOS app-installer mode response")
if value.get("mode") not in {"empty", "legacy-raw", "configured-legacy", "signed-current", "signed-policy-missing", "ambiguous"}:
    raise SystemExit("invalid macOS app-installer mode")
if type(value.get("managed")) is not bool or type(value.get("policy_available")) is not bool:
    raise SystemExit("invalid macOS app-installer mode flags")
print("%s\t%s" % (value["mode"], str(value["managed"] or value["policy_available"]).lower()))
' "$product" <<< "$payload"
}

_macos_app_preflight() {
    local product=$1 root=$2
    local payload
    payload="$(_macos_app_json preflight "$product" "$root")" || return 1
    /usr/bin/python3 -I -B -c '
import json,re,sys
value=json.loads(sys.stdin.read())
product=sys.argv[1]
if not isinstance(value,dict) or value.get("schema_version") != 1 or value.get("product") != product:
    raise SystemExit("invalid macOS app-installer preflight response")
if type(value.get("managed")) is not bool or type(value.get("policy_available")) is not bool:
    raise SystemExit("invalid macOS app-installer preflight flags")
revision=value.get("source_revision", "")
if value.get("mode") == "signed-current" and (not isinstance(revision,str) or not re.fullmatch(r"[0-9a-fA-F]{40}|[0-9a-fA-F]{64}",revision)):
    raise SystemExit("signed preflight lacks an exact source revision")
print(revision)
' "$product" <<< "$payload"
}

_macos_app_verify_current() {
    local product=$1 root=$2
    _macos_app_json verify-current "$product" "$root"
}

_macos_app_install() {
    local product=$1 root=$2 source=$3 version=$4 revision=$5 helper_revision=$6
    _macos_app_json install "$product" "$root" \
        --source "$source" --version "$version" --source-revision "$revision" \
        --helper-source-revision "$helper_revision" >/dev/null
}

_macos_app_service_reconcile() {
    local product=$1 root=$2 dry_run=${3:-false} payload
    [ "$product" = code-intel-daemon ] || {
        echo "ERROR: service reconciliation is only valid for code-intel-daemon" >&2
        return 1
    }
    if [ "$dry_run" = true ]; then
        payload="$(_macos_app_json service-reconcile "$product" "$root" --dry-run)" || return 1
    else
        payload="$(_macos_app_json service-reconcile "$product" "$root")" || return 1
    fi
    MACOS_APP_SERVICE_RECOVERY_PENDING=$(/usr/bin/python3 -I -B -c '
import json,sys
value=json.loads(sys.stdin.read())
if not isinstance(value, dict) or type(value.get("schema_version")) is not int or value.get("schema_version") != 1 or value.get("product") != sys.argv[1] or value.get("mode") != "signed-current" or type(value.get("service_action")) is not str or type(value.get("service_needs_change")) is not bool or type(value.get("published")) is not bool or type(value.get("service_recovery_pending")) is not bool or not isinstance(value.get("plist_path"),str) or not isinstance(value.get("executable_path"),str):
    raise SystemExit("invalid service-reconcile response")
action=value["service_action"]
changed=action in {"restarted","recovered","updated-stopped"}
unchanged=action in {"loaded","stopped","absent"}
preview=action in {"would-restart","would-update-stopped"}
if sys.argv[4] == "true":
    if (preview and (not value["service_needs_change"] or value["published"])) or (not preview and (not unchanged or value["service_needs_change"] or value["published"])):
        raise SystemExit("invalid dry-run service state")
elif not (changed or unchanged) or value["service_needs_change"] != changed or value["published"] != changed:
    raise SystemExit("invalid service-reconcile state")
expected_plist=sys.argv[2]+"/Library/LaunchAgents/com.hex.scipd.plist"
expected_executable=sys.argv[3]+"/SCIPD.app/Contents/MacOS/scipd"
if value["plist_path"] != expected_plist or value["executable_path"] != expected_executable:
    raise SystemExit("service-reconcile paths do not match the fixed owner")
print(str(value["service_recovery_pending"]).lower())
' "$product" "$HOME" "$root" "$dry_run" <<< "$payload") || return 1
}

_macos_app_compatibility_alias() {
    local product=$1 root=$2 workspace=$3 expected_revision=${4:-} payload expected_name expected_alias expected_target
    payload=$(/usr/bin/python3 -I -B "$MACOS_APP_INSTALLER" compatibility-alias "$product" --root "$root" --hex-workspace "$workspace") || return 1
    expected_name=cq
    [ "$product" = code-intel-daemon ] && expected_name=scipd
    expected_alias="$workspace/.hex/bin/$expected_name"
    expected_target="$root/bin/$expected_name"
    /usr/bin/python3 -I -B -c '
import json,sys
value=json.loads(sys.stdin.read())
if not isinstance(value, dict) or value.get("schema_version") != 1 or value.get("product") != sys.argv[1] or not isinstance(value.get("source_revision"),str) or not __import__("re").fullmatch(r"[0-9a-fA-F]{40}|[0-9a-fA-F]{64}",value["source_revision"]) or not isinstance(value.get("generation"),str) or not value["generation"] or value.get("alias_path") != sys.argv[2] or value.get("target_path") != sys.argv[3] or value.get("action") not in {"current", "created", "migrated"} or type(value.get("changed")) is not bool or type(value.get("published")) is not bool:
    raise SystemExit("invalid compatibility-alias response")
changed=value["action"] in {"created","migrated"}
if value["changed"] != changed or value["published"] != changed or (sys.argv[4] and value["source_revision"] != sys.argv[4]):
    raise SystemExit("invalid compatibility-alias state")
' "$product" "$expected_alias" "$expected_target" "$expected_revision" <<< "$payload"
}

_macos_app_recheck() {
    local product=$1 root=$2 managed_at_start=$3
    if ! _macos_app_prepare "$product" "$root"; then
        if [ "$managed_at_start" = true ]; then
            MACOS_APP_MANAGED=true
        fi
        return 1
    fi
    if [ "$managed_at_start" = true ] && [ "$MACOS_APP_MANAGED" != true ]; then
        MACOS_APP_MANAGED=true
        echo "ERROR: managed macOS app state disappeared during $product build; refusing raw fallback" >&2
        return 1
    fi
}

_verify_pinned_checkout() {
    local checkout=$1 tag=$2 expected=$3 actual status
    actual=$(git -C "$checkout" rev-parse HEAD 2>/dev/null) || return 1
    [ "$actual" = "$expected" ] || {
        echo "ERROR: checkout $checkout is not at pinned tag $tag ($expected)" >&2
        return 1
    }
    status=$(git -C "$checkout" status --porcelain --untracked-files=all 2>/dev/null) || {
        echo "ERROR: cannot inspect checkout state: $checkout" >&2
        return 1
    }
    if [ -n "$status" ]; then
        echo "ERROR: checkout $checkout has local changes; refusing build" >&2
        return 1
    fi
}

_macos_app_prepare() {
    local product=$1 root=$2
    MACOS_APP_MODE="legacy-raw"
    MACOS_APP_MANAGED=false
    MACOS_APP_POLICY_AVAILABLE=false
    MACOS_APP_SOURCE_REVISION=""
    MACOS_APP_SERVICE_RECOVERY_PENDING=false
    if ! _macos_app_enabled; then
        return 0
    fi
    if [ ! -f "$MACOS_APP_INSTALLER" ]; then
        echo "ERROR: macOS app-install helper is missing: $MACOS_APP_INSTALLER" >&2
        return 1
    fi
    local mode_result
    mode_result="$(_macos_app_mode "$product" "$root")" || {
        echo "ERROR: macOS app-install mode detection failed for $product" >&2
        return 1
    }
    IFS=$'\t' read -r MACOS_APP_MODE MACOS_APP_POLICY_AVAILABLE <<< "$mode_result"
    case "$MACOS_APP_MODE" in
        configured-legacy|signed-current|signed-policy-missing)
            MACOS_APP_MANAGED=true
            ;;
        empty)
            [ "$MACOS_APP_POLICY_AVAILABLE" = true ] && MACOS_APP_MANAGED=true
            ;;
        legacy-raw)
            ;;
        *)
            echo "ERROR: unknown macOS app-install mode '$MACOS_APP_MODE' for $product" >&2
            return 1
            ;;
    esac
    # Preflight is required before any build or same-version decision. A
    # signed-policy-missing result is an error from the common boundary.
    MACOS_APP_SOURCE_REVISION="$(_macos_app_preflight "$product" "$root")" || {
        echo "ERROR: macOS app-install preflight failed for $product" >&2
        return 1
    }
}

echo "hex v${VERSION} installer"
echo "========================"
echo ""

# ── Phase 1: Validate environment ──────────────────────────────────

echo "Checking prerequisites..."

if ! command -v python3 &>/dev/null; then
    echo "ERROR: Python 3 is required but not found."
    echo "  Install: https://www.python.org/downloads/"
    exit 1
fi

PY_VERSION=$(python3 -c 'import sys; print(f"{sys.version_info.major}.{sys.version_info.minor}")')
PY_MAJOR=$(echo "$PY_VERSION" | cut -d. -f1)
PY_MINOR=$(echo "$PY_VERSION" | cut -d. -f2)
if [ "$PY_MAJOR" -lt 3 ] || { [ "$PY_MAJOR" -eq 3 ] && [ "$PY_MINOR" -lt 9 ]; }; then
    echo "ERROR: Python 3.9+ required (found $PY_VERSION)."
    echo "  Install: https://www.python.org/downloads/"
    exit 1
fi

if ! command -v git &>/dev/null; then
    echo "ERROR: git is required but not found."
    echo "  Install: https://git-scm.com/downloads"
    exit 1
fi

if ! command -v claude &>/dev/null; then
    echo "NOTE: Claude Code CLI not found. Install it to use hex:"
    echo "  npm install -g @anthropic-ai/claude-code"
    echo ""
fi

if [ -d "$TARGET_DIR" ]; then
    echo "ERROR: $TARGET_DIR already exists."
    echo "  To upgrade:   bash \"$TARGET_DIR/.hex/scripts/upgrade.sh\""
    echo "  To reinstall: rm -rf \"$TARGET_DIR\" && bash install.sh"
    exit 1
fi

echo "  Python $PY_VERSION  ✓"
echo "  git               ✓"
echo ""

# ── ZONES — Core vs user-space ─────────────────────────────────────
#
# CORE (overwritten by hex upgrade):
#   $TARGET_DIR/.hex/           ← installed from system/ in hex-foundation repo
#
# USER SPACE (never touched by hex upgrade):
#   $TARGET_DIR/.hex/extensions/  ← user-installed extensions
#   $TARGET_DIR/projects/
#   $TARGET_DIR/me/
#   $TARGET_DIR/evolution/
#   $TARGET_DIR/templates/
#   $TARGET_DIR/integrations/
#   $TARGET_DIR/extensions/
#
# hex upgrade writes only to the core zone. User space is preserved.
# See ZONES.md in the hex-foundation repo for the full boundary spec.

# ── Phase 2: Create instance directory structure ───────────────────

echo "Creating hex instance at $TARGET_DIR..."

mkdir -p "$TARGET_DIR"/{me/decisions,projects/_archive,people}
mkdir -p "$TARGET_DIR"/evolution
mkdir -p "$TARGET_DIR"/landings/weekly
mkdir -p "$TARGET_DIR"/raw/{transcripts,handoffs}
mkdir -p "$TARGET_DIR"/specs/_archive

# Copy system files → .hex/   (CORE zone)
# This bulk cp covers EVERY system/* path including system/telemetry/migrations/
# (the C3 baseline VIEW migrations) and system/scripts/, system/skills/, etc.
# If you ever break this bulk cp into per-subdir copies, system/telemetry/migrations
# MUST remain covered — it carries the C3 metric VIEW DDL applied by
# telemetry-init.sh. Refactor guard: keep the literal string
# `system/telemetry/migrations` mentioned here so OBS-025 / Plan A v4-final's
# install.sh verify-only check stays green across future refactors.
cp -r "$SCRIPT_DIR/system" "$TARGET_DIR/.hex"

# Create user-space extensions directory (never overwritten by hex upgrade)
mkdir -p "$TARGET_DIR/.hex/extensions"

# Create memory directory for markdown-format memories
mkdir -p "$TARGET_DIR/.hex/memory"

# Copy root templates
cp "$SCRIPT_DIR/templates/AGENTS.md"  "$TARGET_DIR/AGENTS.md"
ln -sfn AGENTS.md "$TARGET_DIR/CLAUDE.md"
cp "$SCRIPT_DIR/templates/todo.md"    "$TARGET_DIR/todo.md"

# Copy user data templates
cp "$SCRIPT_DIR/templates/me.md"            "$TARGET_DIR/me/me.md"
cp "$SCRIPT_DIR/templates/learnings.md"     "$TARGET_DIR/me/learnings.md"
cp "$SCRIPT_DIR/templates/observations.md"  "$TARGET_DIR/evolution/observations.md"
cp "$SCRIPT_DIR/templates/suggestions.md"   "$TARGET_DIR/evolution/suggestions.md"
cp "$SCRIPT_DIR/templates/changelog.md"     "$TARGET_DIR/evolution/changelog.md"

# Create evolution/eval dir (session-delta.py was ported to Rust in
# session_reflect.rs — commit a819261f / BOI S8785 — and the template
# was deleted. Dir kept for downstream tools that expect it.)
mkdir -p "$TARGET_DIR/evolution/eval"

# Copy tests
if [ -d "$SCRIPT_DIR/tests" ]; then
    cp -r "$SCRIPT_DIR/tests" "$TARGET_DIR/tests"
fi

# Copy commands to both .claude/commands/ (Claude Code) and .hex/commands/ (doctor/tooling)
if [ -d "$SCRIPT_DIR/system/commands" ]; then
    mkdir -p "$TARGET_DIR/.claude/commands"
    cp "$SCRIPT_DIR/system/commands/"*.md "$TARGET_DIR/.claude/commands/"
    mkdir -p "$TARGET_DIR/.hex/commands"
    cp "$SCRIPT_DIR/system/commands/"*.md "$TARGET_DIR/.hex/commands/"
fi

# Symlink .agents/skills/ → .hex/skills/ so tools that look in .agents/ find the same skill set
mkdir -p "$TARGET_DIR/.agents"
ln -sfn ../.hex/skills "$TARGET_DIR/.agents/skills"

# Seed optional configs doctor expects. Defaults are safe and overridable later.
echo '{}' > "$TARGET_DIR/.hex/settings.json"

# Copy hook scripts and configure Claude Code hooks in .claude/settings.json
HOOKS_MANIFEST="$SCRIPT_DIR/system/hooks/required-hooks.json"
if [ -d "$SCRIPT_DIR/system/hooks/scripts" ]; then
    mkdir -p "$TARGET_DIR/.hex/hooks/scripts"
    cp "$SCRIPT_DIR/system/hooks/scripts/"* "$TARGET_DIR/.hex/hooks/scripts/" 2>/dev/null || true
    chmod +x "$TARGET_DIR/.hex/hooks/scripts/"*.sh 2>/dev/null || true
fi
if [ -f "$HOOKS_MANIFEST" ]; then
    mkdir -p "$TARGET_DIR/.claude"
    MANIFEST_PATH="$HOOKS_MANIFEST" SETTINGS_PATH="$TARGET_DIR/.claude/settings.json" python3 << 'PYEOF'
import json, os

manifest_path = os.environ['MANIFEST_PATH']
settings_path = os.environ['SETTINGS_PATH']

with open(manifest_path) as f:
    manifest = json.load(f)

if os.path.exists(settings_path):
    with open(settings_path) as f:
        try:
            settings = json.load(f)
        except json.JSONDecodeError:
            settings = {}
else:
    settings = {}

if 'hooks' not in settings:
    settings['hooks'] = {}

hooks_section = settings['hooks']

for event_type, hook_defs in manifest.items():
    if event_type not in hooks_section:
        hooks_section[event_type] = []
    event_hooks = hooks_section[event_type]
    for hook_def in hook_defs:
        matcher = hook_def.get('matcher', '')
        if 'command' in hook_def:
            hook_command = hook_def['command']
            is_present = any(
                any(h.get('command', '') == hook_command for h in entry.get('hooks', []))
                for entry in event_hooks
            )
        else:
            script_rel = hook_def['script']
            script_name = os.path.basename(script_rel)
            hook_command = f'bash "$CLAUDE_PROJECT_DIR/{script_rel}"'
            is_present = any(
                any(script_name in h.get('command', '') for h in entry.get('hooks', []))
                for entry in event_hooks
            )
        if not is_present:
            event_hooks.append({
                'matcher': matcher,
                'hooks': [{'type': 'command', 'command': hook_command}]
            })

tmp = settings_path + '.tmp'
os.makedirs(os.path.dirname(tmp), exist_ok=True)
with open(tmp, 'w') as f:
    json.dump(settings, f, indent=2)
os.replace(tmp, settings_path)
PYEOF
    echo "  Claude Code hooks   ✓"
fi

# env.sh is already copied from system/scripts/env.sh via the cp -r above.
# Make it executable.
chmod +x "$TARGET_DIR/.hex/scripts/env.sh"
echo "  env.sh              ✓"
if [ -L /etc/localtime ]; then
    # /etc/localtime → /var/db/timezone/zoneinfo/America/Los_Angeles → America/Los_Angeles
    readlink /etc/localtime 2>/dev/null | sed 's|.*zoneinfo/||' > "$TARGET_DIR/.hex/timezone"
fi
# If detection failed or produced empty, leave the file absent (doctor will warn but not error)
if [ -f "$TARGET_DIR/.hex/timezone" ] && [ ! -s "$TARGET_DIR/.hex/timezone" ]; then
    rm -f "$TARGET_DIR/.hex/timezone"
fi

# Initialize the instance as a git repo so decision logs, landings, and
# me/ evolve with history. Quiet failure mode: skip if git init fails.
( cd "$TARGET_DIR" && git init -q 2>/dev/null && git add -A 2>/dev/null && \
    git -c user.email=hex@local -c user.name=hex commit -q -m "hex v${VERSION} initial install" 2>/dev/null ) || true

echo "  Directory structure  ✓"

# ── Phase 3: Initialize memory ─────────────────────────────────────

echo "Initializing memory database..."

python3 -c "
import sqlite3, os
db = os.path.join('$TARGET_DIR', '.hex', 'memory.db')
conn = sqlite3.connect(db)
conn.executescript('''
    CREATE TABLE IF NOT EXISTS memories (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        content TEXT NOT NULL,
        tags TEXT DEFAULT \"\",
        source TEXT DEFAULT \"\",
        created_at TEXT NOT NULL
    );
    CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
        content, tags, source,
        content=memories, content_rowid=id,
        tokenize=\"unicode61\"
    );
    CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
        INSERT INTO memories_fts(rowid, content, tags, source)
        VALUES (new.id, new.content, new.tags, new.source);
    END;
    CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
        INSERT INTO memories_fts(memories_fts, rowid, content, tags, source)
        VALUES (\"delete\", old.id, old.content, old.tags, old.source);
    END;
    CREATE VIRTUAL TABLE IF NOT EXISTS chunks USING fts5(
        source_path, heading, chunk_index, content,
        tokenize=\"unicode61\"
    );
    CREATE TABLE IF NOT EXISTS files (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        path TEXT UNIQUE NOT NULL,
        mtime REAL NOT NULL,
        content_hash TEXT NOT NULL DEFAULT \"\",
        indexed_at TEXT NOT NULL,
        chunk_count INTEGER DEFAULT 0
    );
    CREATE TABLE IF NOT EXISTS metadata (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
''')
conn.commit()
conn.close()
"

echo "  Memory database     ✓"

# ── Phase 4: Create standing-orders reference ──────────────────────

mkdir -p "$TARGET_DIR/.hex/standing-orders"
cat > "$TARGET_DIR/.hex/standing-orders/README.md" << 'SOEOF'
# Standing Orders

The 20 core rules, 10 situational rules, and 6 product judgment rules are
defined in CLAUDE.md (system zone). This directory holds extended reference
copies with examples and context for each rule.

See CLAUDE.md → Standing Orders for the working copy.
SOEOF

echo "  Standing orders     ✓"

# ── Phase 5: Install companions ────────────────────────────────────

echo "Installing companions..."

# Memory hybrid-search deps (optional — FTS5-only mode if pip fails)
MEMORY_REQS="$SCRIPT_DIR/system/skills/memory/requirements.txt"
if [ -f "$MEMORY_REQS" ]; then
    if python3 -m pip install -q -r "$MEMORY_REQS" 2>/dev/null; then
        echo "  Memory hybrid deps  ✓"
    else
        echo "  ⚠️  Memory hybrid deps skipped — memory will use FTS5-only mode"
    fi
fi

# Read pinned versions from VERSIONS file (keeps install.sh in lock-step with
# tested boi releases). Fork-friendly: the HEX_BOI_REPO env var overrides the
# default source.
VERSIONS_FILE="$SCRIPT_DIR/VERSIONS"
if [ ! -f "$VERSIONS_FILE" ]; then
    echo "ERROR: $VERSIONS_FILE not found — this hex-foundation checkout is incomplete."
    exit 1
fi
BOI_VERSION=$(grep "^BOI_VERSION=" "$VERSIONS_FILE" | cut -d= -f2)
HARNESS_VERSION=$(grep "^HARNESS_VERSION=" "$VERSIONS_FILE" | cut -d= -f2 || true)
BOI_REPO="${HEX_BOI_REPO:-https://github.com/mrap/boi.git}"
HEX_REPO="${HEX_FOUNDATION_REPO:-https://github.com/mrap/hex-foundation.git}"

_resolve_git_tag() {
    local repo=$1 tag=$2 refs sha
    refs=$(git ls-remote "$repo" "refs/tags/$tag^{}" "refs/tags/$tag" 2>/dev/null) || return 1
    sha=$(printf '%s\n' "$refs" | awk -v peeled="refs/tags/$tag^{}" -v direct="refs/tags/$tag" '$2 == peeled { print $1; exit }')
    [ -n "$sha" ] || sha=$(printf '%s\n' "$refs" | awk -v direct="refs/tags/$tag" '$2 == direct { print $1; exit }')
    /usr/bin/python3 -I -B -c 'import re,sys; raise SystemExit(0 if re.fullmatch(r"(?:[0-9a-fA-F]{40}|[0-9a-fA-F]{64})", sys.argv[1] or "") else 1)' "$sha" || return 1
    printf '%s\n' "$sha"
}

# Read-only consumer for the one-root managed Cargo target contract. It is used
# only by the existing managed macOS source-build paths below. The adapter owns
# policy parsing, alias resolution, and installed-checker receipt validation.
# That receipt is not installed-checker provenance attestation; the rollout
# owner verifies installed authority separately. This consumer owns the
# create/recheck boundary immediately before Cargo.
_managed_target_receipt() {
    local requested_target=$1 source_revision=$2
    local python_bin="${MANAGED_TARGET_PYTHON:-/usr/bin/python3}"
    local adapter="$SCRIPT_DIR/system/scripts/managed-target-check.py"
    local receipt parsed
    local -a adapter_args
    [ -f "$adapter" ] || { echo "ERROR: managed target adapter is missing: $adapter" >&2; return 1; }
    adapter_args=(--caller foundation-install --executable "$SCRIPT_DIR/install.sh" --source-revision "$source_revision")
    [ -z "$requested_target" ] || adapter_args+=(--target "$requested_target")
    receipt=$("$python_bin" -I -B "$adapter" "${adapter_args[@]}") || {
        echo "ERROR: managed target first or recheck was rejected" >&2
        return 1
    }
    parsed=$("$python_bin" -I -B -c '
import hashlib,json,os,re,sys
executable, source_revision, requested_target = sys.argv[1:]
try:
    value=json.loads(sys.stdin.read())
    canonical=os.path.realpath(executable)
    with open(canonical,"rb") as handle: digest=hashlib.sha256(handle.read()).hexdigest()
    required={"schema_version","status","caller_identity","executable_identity","resolved_target","policy_revision","source_revision","selection_source"}
    identity=value.get("executable_identity")
    expected_selection=("ARGUMENT" if requested_target else ("CARGO_TARGET_DIR" if "CARGO_TARGET_DIR" in os.environ else ("BOI_CARGO_TARGET_DIR" if "BOI_CARGO_TARGET_DIR" in os.environ else "DAEMON_TOML")))
    valid=(isinstance(value,dict) and set(value)==required and value.get("schema_version")=="boi.managed-target-check.v1" and value.get("status")=="accepted" and value.get("caller_identity")=="foundation-install" and value.get("source_revision")==source_revision and value.get("selection_source")==expected_selection and isinstance(identity,dict) and set(identity)=={"canonical_path","sha256"} and identity.get("canonical_path")==canonical and identity.get("sha256")==digest and isinstance(value.get("resolved_target"),str) and os.path.isabs(value["resolved_target"]) and isinstance(value.get("policy_revision"),str) and re.fullmatch(r".+:sha256:[0-9a-f]{64}",value["policy_revision"],re.S))
    if not valid or any(char in value["resolved_target"] for char in "\t\r\n") or any(char in value["policy_revision"] for char in "\t\r\n"):
        raise ValueError("receipt does not bind this installer request")
    print(value["resolved_target"]+"\t"+value["policy_revision"])
except Exception as exc:
    raise SystemExit("invalid managed target receipt: %s" % exc)
' "$SCRIPT_DIR/install.sh" "$source_revision" "$requested_target" <<< "$receipt") || {
        echo "ERROR: managed target receipt is invalid" >&2
        return 1
    }
    printf '%s\n' "$parsed"
}

_managed_cargo_target_precheck() {
    local requested_target=$1 source_revision=$2 internal_build_dir="${3:-false}"
    local python_bin="${MANAGED_TARGET_PYTHON:-/usr/bin/python3}"
    local adapter="$SCRIPT_DIR/system/scripts/managed-target-check.py"
    local first first_target first_policy caller_build_dir
    first=$(_managed_target_receipt "$requested_target" "$source_revision") || return 1
    IFS=$'\t' read -r first_target first_policy <<< "$first"
    if [ -n "${CARGO_BUILD_BUILD_DIR+x}" ] && [ "$internal_build_dir" != true ]; then
        caller_build_dir="$CARGO_BUILD_BUILD_DIR"
        "$python_bin" -I -B -c '
import importlib.util,sys
spec=importlib.util.spec_from_file_location("managed_target_check",sys.argv[1])
module=importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
module.validate_same_root_build_dir(sys.argv[2],sys.argv[3])
' "$adapter" "$caller_build_dir" "$first_target" || {
            echo "ERROR: caller CARGO_BUILD_BUILD_DIR does not match the accepted target" >&2
            return 1
        }
    fi
    MANAGED_CARGO_TARGET="$first_target"
    MANAGED_CARGO_POLICY_REVISION="$first_policy"
}

_managed_cargo_target() {
    local requested_target=$1 source_revision=$2 internal_build_dir="${3:-false}" require_new="${4:-false}"
    local second first_target first_policy second_target second_policy created=false
    _managed_cargo_target_precheck "$requested_target" "$source_revision" "$internal_build_dir" || return 1
    first_target="$MANAGED_CARGO_TARGET"
    first_policy="$MANAGED_CARGO_POLICY_REVISION"
    if [ -e "$first_target" ]; then
        [ "$require_new" != true ] || {
            echo "ERROR: installer-owned managed target already exists: $first_target" >&2
            return 1
        }
    else
        mkdir "$first_target" || { echo "ERROR: could not create accepted managed target: $first_target" >&2; return 1; }
        created=true
    fi
    second=$(_managed_target_receipt "$first_target" "$source_revision") || {
        if [ "$created" = true ]; then
            if rmdir "$first_target"; then
                echo "ERROR: managed target recheck failed; removed empty created target" >&2
            else
                echo "ERROR: managed target recheck failed; retained created target: $first_target" >&2
            fi
        fi
        return 1
    }
    IFS=$'\t' read -r second_target second_policy <<< "$second"
    if [ "$second_target" != "$first_target" ] || [ "$second_policy" != "$first_policy" ]; then
        if [ "$created" = true ]; then
            if rmdir "$first_target"; then
                echo "ERROR: managed target changed during recheck; removed empty created target" >&2
            else
                echo "ERROR: managed target changed during recheck; retained created target: $first_target" >&2
            fi
        fi
        echo "ERROR: managed target changed during recheck" >&2
        return 1
    fi
    MANAGED_CARGO_TARGET="$second_target"
    export CARGO_TARGET_DIR="$second_target"
    export CARGO_BUILD_BUILD_DIR="$second_target"
    MANAGED_CARGO_BUILD_DIR_INTERNAL=true
}

# BOI — parallel worker dispatch (boi-v2: the canonical TOML engine).
# Builds in a MACHINE-OWNED clone under ~/.boi/src/boi and never touches a
# developer checkout (e.g. ~/github.com/mrap/boi). The previous version ran
# `checkout -f $TAG` + `checkout -B main` against the developer repo, which
# force-reset its main to the pinned tag on every install/upgrade/test run —
# silently eating merged work 4x (OBS-033, 2026-06-10). The build
# checkout stays detached at the pinned tag: it is an artifact cache, not a
# working repo, so there is no branch to leave behind.

# boi.sh wrapper for shell aliases — lives next to the binary, not in any repo.
write_boi_wrapper() {
    cat > "$HOME/.boi/bin/boi.sh" << 'BOISH'
#!/bin/bash
if [ -x "$HOME/.boi/bin/boi" ]; then
    exec "$HOME/.boi/bin/boi" "$@"
fi
echo "error: BOI binary not found at ~/.boi/bin/boi"
exit 1
BOISH
    chmod +x "$HOME/.boi/bin/boi.sh"
}

install_or_upgrade_boi() {
    local boi_build="$HOME/.boi/src/boi"
    local boi_bin="$HOME/.boi/bin/boi"
    if ! _macos_app_prepare boi "$HOME/.boi"; then
        return 1
    fi
    local boi_managed_at_start="$MACOS_APP_MANAGED"
    local boi_target_dir=""
    local pinned_boi_revision=""
    # Fast path: the machine-owned build already provides the pinned version.
    # (Also makes repeated install.sh runs — e.g. from test suites — no-ops.)
    # Raw installs use a real-file copy via atomic rename so a rebuild never
    # overwrites the Mach-O mapped by a live daemon. Managed signed installs
    # may expose the transaction-owned compatibility symlink. A present but
    # unrunnable binary falls through to the rebuild below.
    if [ -x "$boi_bin" ]; then
        local fast_path=false current=""
        if [ "$MACOS_APP_MANAGED" = true ]; then
            if [ "$MACOS_APP_MODE" = signed-current ]; then
                local precheck_revision
                precheck_revision=$(git -C "$SCRIPT_DIR" rev-parse HEAD) || return 1
                _managed_cargo_target_precheck "${CARGO_TARGET_DIR:-}" "$precheck_revision" || return 1
                pinned_boi_revision=$(_resolve_git_tag "$BOI_REPO" "$BOI_VERSION") || {
                    echo "ERROR: managed BOI install requires a resolvable pinned source tag $BOI_VERSION; refusing raw fallback" >&2
                    return 1
                }
                local verified_revision verified_version verified_metadata verified_fields
                verified_metadata="$(_macos_app_verify_current boi "$HOME/.boi")" || {
                    echo "ERROR: signed BOI installation failed verify-current; refusing raw fast path" >&2
                    return 1
                }
                verified_fields=$(/usr/bin/python3 -I -B -c '
import json,re,sys
value=json.loads(sys.stdin.read())
if not isinstance(value, dict) or type(value.get("schema_version")) is not int or value.get("schema_version") != 1 or value.get("product") != "boi" or value.get("mode") != "signed-current":
    raise SystemExit("invalid verified BOI metadata")
revision=value.get("source_revision")
version=value.get("version")
if not isinstance(revision, str) or not re.fullmatch(r"(?:[0-9a-fA-F]{40}|[0-9a-fA-F]{64})", revision) or not isinstance(version, str) or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
    raise SystemExit("invalid verified BOI source revision or version")
print("%s\t%s" % (revision, version))
' <<< "$verified_metadata") || {
                    echo "ERROR: signed BOI metadata is invalid; refusing raw fast path" >&2
                    return 1
                }
                IFS=$'\t' read -r verified_revision verified_version <<< "$verified_fields"
                if [ "$verified_revision" = "$pinned_boi_revision" ] && [ "$verified_version" = "${BOI_VERSION#v}" ]; then
                    fast_path=true
                else
                    echo "  BOI signed state differs from the pinned source; rebuilding through the common transaction"
                fi
            fi
        elif [ ! -L "$boi_bin" ]; then
            current="v$("$boi_bin" --version 2>/dev/null | awk '/^boi /{print $2}' | tail -1 || true)"
            [ "$current" = "$BOI_VERSION" ] && fast_path=true
        fi
        if [ "$fast_path" = true ]; then
            echo "  BOI $BOI_VERSION already installed  ✓"
            write_boi_wrapper
            return 0
        fi
    fi

    if [ "$boi_managed_at_start" = true ]; then
        local caller_status caller_revision requested_boi_target
        caller_status=$(git -C "$SCRIPT_DIR" status --porcelain --untracked-files=all) || return 1
        [ -z "$caller_status" ] || { echo "ERROR: source checkout is dirty; refusing managed BOI build" >&2; return 1; }
        caller_revision=$(git -C "$SCRIPT_DIR" rev-parse HEAD) || return 1
        requested_boi_target="${CARGO_TARGET_DIR:-}"
        _managed_cargo_target "$requested_boi_target" "$caller_revision" || return 1
        boi_target_dir="$MANAGED_CARGO_TARGET"
        pinned_boi_revision=$(_resolve_git_tag "$BOI_REPO" "$BOI_VERSION") || {
            echo "ERROR: managed BOI install requires a resolvable pinned source tag $BOI_VERSION; refusing raw fallback" >&2
            return 1
        }
    fi

    mkdir -p "$HOME/.boi/bin" "$HOME/.boi/pids" "$HOME/.boi/logs" \
             "$HOME/.boi/worktrees" "$HOME/.boi/src"

    # TRIPWIRE (2026-06-05): record who triggers the boi rebuild/symlink loop.
    # Kept: it identified the OBS-033 resetter (codex-parity tests → install.sh).
    {
        echo "[$(date '+%F %T')] install_or_upgrade_boi BOI_VERSION=$BOI_VERSION pid=$$ ppid=$PPID"
        ps -o pid,ppid,command -p "$PPID" 2>/dev/null || true
        echo "  args: $0 $*"
    } >> "$HOME/.boi/install-tripwire.log" 2>&1 || true

    # Update the machine-owned build checkout (detached at the tag). A repo
    # that cannot reach the pin (corrupt clone, force-moved tag) self-heals by
    # re-cloning fresh — never build a stale checkout and call it $BOI_VERSION.
    # (fetch failure alone is tolerated: the pinned tag may already be local.)
    if [ -d "$boi_build/.git" ]; then
        echo "  BOI build repo exists — fetching $BOI_VERSION..."
        if ! ( cd "$boi_build" && { git fetch --tags origin 2>/dev/null || true; } && \
               git checkout -f --detach "$BOI_VERSION" 2>/dev/null ); then
            echo "  BOI: build repo cannot reach $BOI_VERSION — re-cloning fresh" >&2
            rm -rf "$boi_build"
        fi
    fi
    if [ ! -d "$boi_build/.git" ]; then
        echo "  Cloning BOI build repo (machine-owned, ~/.boi/src/boi)..."
        git clone "$BOI_REPO" "$boi_build" 2>/dev/null || {
            echo "  BOI: failed to clone $BOI_REPO — keeping currently installed binary" >&2
            return 1
        }
        ( cd "$boi_build" && git checkout -f --detach "$BOI_VERSION" 2>/dev/null ) || {
            echo "  BOI: tag $BOI_VERSION not found in $BOI_REPO — keeping currently installed binary" >&2
            return 1
        }
    fi
    local resolved_boi_revision
    resolved_boi_revision="$(_resolve_git_tag "$BOI_REPO" "$BOI_VERSION")" || {
        echo "  BOI: pinned tag $BOI_VERSION could not be resolved after checkout" >&2
        return 1
    }
    _verify_pinned_checkout "$boi_build" "$BOI_VERSION" "$resolved_boi_revision" || return 1
    if [ "$boi_managed_at_start" = true ] && [ "$resolved_boi_revision" != "$pinned_boi_revision" ]; then
        echo "ERROR: BOI checkout does not match the managed pinned revision; refusing signed build" >&2
        return 1
    fi

    # Build the Rust binary (full log kept — a swallowed compiler error makes
    # failures undiagnosable, S6)
    if command -v cargo &>/dev/null; then
        echo "  Building BOI binary..."
        local build_log="$HOME/.boi/logs/boi-build.log"
        if [ "$boi_managed_at_start" != true ]; then
            boi_target_dir="${CARGO_TARGET_DIR:-$HOME/.boi/cargo-target}"
            if [[ "$boi_target_dir" != /* ]]; then boi_target_dir="$boi_build/$boi_target_dir"; fi
            boi_target_dir="$(mkdir -p "$boi_target_dir" && cd "$boi_target_dir" && pwd -P)" || return 1
        fi
        ( cd "$boi_build" && CARGO_TARGET_DIR="$boi_target_dir" cargo build --release ) > "$build_log" 2>&1 || {
            echo "  BOI: cargo build failed — last 20 lines of $build_log:" >&2
            tail -20 "$build_log" >&2 || true
            return 1
        }
        # Deploy as a STABLE real file via atomic rename — never symlink to (or
        # overwrite in place) the build output the running daemon is mapped from
        # (FIX-017). The copy gets boi_bin a fresh inode; rename(2) is atomic, so
        # a live daemon keeps its old inode alive until its next restart instead
        # of being AMFI-SIGKILLed ("Code Signature Invalid"). Temp on the SAME
        # filesystem (sibling path) so the rename cannot fall back to a copy.
        local built_boi="$boi_target_dir/release/boi"
        if [ ! -x "$built_boi" ]; then
            echo "  BOI: expected build artifact missing: $built_boi" >&2
            return 1
        fi
        if ! _macos_app_recheck boi "$HOME/.boi" "$boi_managed_at_start"; then
            return 1
        fi
        if [ "$MACOS_APP_MANAGED" = true ]; then
            local boi_revision
            boi_revision="$pinned_boi_revision"
            if [ -z "$boi_revision" ]; then
                echo "  BOI: pinned source revision unavailable; refusing signed install" >&2
                return 1
            fi
            local boi_helper_revision
            boi_helper_revision=$(git -C "$SCRIPT_DIR" rev-parse HEAD) || {
                echo "  BOI: installer helper revision unavailable; refusing signed install" >&2
                return 1
            }
            _macos_app_install boi "$HOME/.boi" "$built_boi" "${BOI_VERSION#v}" "$boi_revision" "$boi_helper_revision" || {
                echo "  BOI: common signed app transaction failed; inspect its transaction result; no raw fallback was attempted" >&2
                return 1
            }
            echo "  BOI $BOI_VERSION built and installed through signed app transaction  ✓"
        else
            local boi_tmp="$boi_bin.new.$$"
            cp -f "$built_boi" "$boi_tmp" && chmod +x "$boi_tmp" && mv -f "$boi_tmp" "$boi_bin" || {
                echo "  BOI: failed to install built binary to $boi_bin" >&2
                rm -f "$boi_tmp" 2>/dev/null || true
                return 1
            }
            echo "  BOI $BOI_VERSION built and installed (atomic)  ✓"
        fi
    else
        echo "  ⚠️  Rust/cargo not found — cannot build BOI binary"
        echo "     Install Rust: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
        return 1
    fi

    write_boi_wrapper
}
install_or_upgrade_boi

# ── Phase 5: Register install ──────────────────────────────────────

python3 -c "
import json, os
from datetime import datetime, timezone
info = {
    'install_path': '$TARGET_DIR',
    'install_date': datetime.now(timezone.utc).isoformat(),
    'version': '$VERSION'
}
with open(os.path.expanduser('~/.hex-install.json'), 'w') as f:
    json.dump(info, f, indent=2)
"

# Seed optional configs (llm-preference, codex config) via doctor's --fix path.
# HEX_DIR must be set explicitly so doctor.sh doesn't auto-detect the caller's cwd.
# Silent; any failure is non-fatal.
HEX_DIR="$TARGET_DIR" bash "$TARGET_DIR/.hex/scripts/doctor.sh" --fix --quiet >/dev/null 2>&1 || true

# ── Phase 7: Install hex binary (unified harness + server) ────────

echo "Installing hex binary..."

mkdir -p "$TARGET_DIR/.hex/bin"
mkdir -p "$TARGET_DIR/.hex/data"
mkdir -p "$TARGET_DIR/.hex/sse/topics"

_harness_build_from_source() {
    if ! _macos_app_prepare hex "$TARGET_DIR/.hex"; then
        return 1
    fi
    local hex_managed_at_start="$MACOS_APP_MANAGED"
    local codeintel_cli_mode_at_start codeintel_daemon_mode_at_start
    local codeintel_cli_managed_at_start codeintel_daemon_managed_at_start
    local codeintel_cli_revision_at_start codeintel_daemon_revision_at_start
    _macos_app_prepare code-intel-cli "$HOME/.codeintel" || return 1
    codeintel_cli_mode_at_start="$MACOS_APP_MODE"
    codeintel_cli_managed_at_start="$MACOS_APP_MANAGED"
    codeintel_cli_revision_at_start="${MACOS_APP_SOURCE_REVISION:-}"
    _macos_app_prepare code-intel-daemon "$HOME/.codeintel" || return 1
    codeintel_daemon_mode_at_start="$MACOS_APP_MODE"
    codeintel_daemon_managed_at_start="$MACOS_APP_MANAGED"
    codeintel_daemon_revision_at_start="${MACOS_APP_SOURCE_REVISION:-}"
    if [ "$codeintel_cli_mode_at_start" = signed-policy-missing ] || [ "$codeintel_daemon_mode_at_start" = signed-policy-missing ]; then
        echo "ERROR: code-intel signed policy is missing; refusing companion publication" >&2
        return 1
    fi
    local source_revision source_status
    source_status=$(git -C "$SCRIPT_DIR" status --porcelain --untracked-files=all) || return 1
    if [ -n "$source_status" ]; then
        echo "ERROR: source checkout is dirty; refusing managed build" >&2
        return 1
    fi
    source_revision=$(git -C "$SCRIPT_DIR" rev-parse HEAD) || return 1
    echo "  Building hex from source..."
    local hex_target_dir="${CARGO_TARGET_DIR:-$TARGET_DIR/.hex/cargo-target}"
    if [ "$hex_managed_at_start" = true ]; then
        _managed_cargo_target "${CARGO_TARGET_DIR:-}" "$source_revision" || return 1
        hex_target_dir="$MANAGED_CARGO_TARGET"
    else
        if [[ "$hex_target_dir" != /* ]]; then hex_target_dir="$SCRIPT_DIR/$hex_target_dir"; fi
        hex_target_dir="$(mkdir -p "$hex_target_dir" && cd "$hex_target_dir" && pwd -P)" || return 1
    fi
    ( cd "$SCRIPT_DIR/system/harness" && CARGO_TARGET_DIR="$hex_target_dir" cargo build --release 2>&1 ) || return 1
    local source_revision_after source_status_after
    source_status_after=$(git -C "$SCRIPT_DIR" status --porcelain --untracked-files=all) || return 1
    source_revision_after=$(git -C "$SCRIPT_DIR" rev-parse HEAD) || return 1
    if [ -n "$source_status_after" ] || [ "$source_revision_after" != "$source_revision" ]; then
        echo "ERROR: source checkout changed during build; refusing publication" >&2
        return 1
    fi
    # Cargo receives an absolute target directory, so the artifact lookup uses
    # the exact output path and cannot select a stale worktree artifact.
    local built=""
    built="$hex_target_dir/release/hex"
    if [ ! -x "$built" ]; then
        echo "  hex binary not found at the exact build output: $built" >&2
        return 1
    fi
    if ! _macos_app_recheck hex "$TARGET_DIR/.hex" "$hex_managed_at_start"; then
        return 1
    fi
    if [ "$MACOS_APP_MANAGED" = true ]; then
        local hex_revision
        hex_revision=$(git -C "$SCRIPT_DIR" rev-parse HEAD) || {
            echo "  hex: source revision unavailable; refusing signed install" >&2
            return 1
        }
        local hex_helper_revision
        hex_helper_revision=$(git -C "$SCRIPT_DIR" rev-parse HEAD) || {
            echo "  hex: installer helper revision unavailable; refusing signed install" >&2
            return 1
        }
        _macos_app_install hex "$TARGET_DIR/.hex" "$built" "${VERSION#v}" "$hex_revision" "$hex_helper_revision" || {
            echo "  hex: common signed app transaction failed; inspect its transaction result; no raw fallback was attempted" >&2
            return 1
        }
    else
        cp "$built" "$TARGET_DIR/.hex/bin/hex"
        chmod +x "$TARGET_DIR/.hex/bin/hex"
        ln -sf hex "$TARGET_DIR/.hex/bin/hex-agent"
        # Record the source SHA that produced THIS binary (atomic tmp+rename) so
        # `hex upgrade` can verify binary freshness. Never fails the install (S6).
        write_hex_sha_sidecar
    fi
    _code_intel_build_and_deploy "$hex_target_dir" "$codeintel_cli_managed_at_start" "$codeintel_daemon_managed_at_start" "$source_revision" "$codeintel_cli_mode_at_start" "$codeintel_daemon_mode_at_start" "$codeintel_cli_revision_at_start" "$codeintel_daemon_revision_at_start" || {
        if [ "$MACOS_APP_MANAGED" = true ]; then
            echo "ERROR: code-intel companion transaction failed after Hex was installed; Hex is already installed" >&2
        fi
        return 1
    }
    return 0
}

# Build + deploy the code-intel binaries (cq, scipd). system/code-intel is a
# workspace sibling of system/harness; the harness depends on it via
# `scipd = { path = "../code-intel" }`, and the bulk `cp -r system → .hex`
# above already lands its SOURCE at .hex/code-intel so the synced
# .hex/harness/Cargo.toml resolves. This step deploys the BINARIES alongside
# hex. Best-effort: a failure here must not fail the hex install (and must not
# trigger the prebuilt-hex download fallback) — warn loudly and move on.
_code_intel_build_and_deploy() {
    local target_dir="${1:-${CARGO_TARGET_DIR:-$TARGET_DIR/.hex/cargo-target}}"
    local cli_managed="${2:-false}" daemon_managed="${3:-false}" source_revision="${4:-}"
    local cli_mode="${5:-}" daemon_mode="${6:-}" cli_revision="${7:-}" daemon_revision="${8:-}"
    if [ ! -f "$SCRIPT_DIR/system/code-intel/Cargo.toml" ]; then
        if [ "$cli_managed" = true ] || [ "$daemon_managed" = true ]; then
            echo "ERROR: managed code-intel state exists but Cargo.toml is missing" >&2
            return 1
        fi
        return 0
    fi
    local version
    version=$(/usr/bin/python3 -I -B -c 'import re,sys; text=open(sys.argv[1]).read(); block=re.search(r"(?ms)^\[package\]\s*(.*?)(?=^\[|\Z)",text); match=re.search(r"^version\s*=\s*\"([^\"]+)\"\s*$",block.group(1),re.M) if block else None; sys.stdout.write(match.group(1)+"\n") if match else sys.exit("missing package version")' "$SCRIPT_DIR/system/code-intel/Cargo.toml") || return 1
    if [ "$cli_managed" = true ] || [ "$daemon_managed" = true ]; then
        [ "$cli_managed" = true ] && [ "$daemon_managed" = true ] || {
            echo "ERROR: code-intel products have inconsistent managed state; refusing partial publication" >&2
            return 1
        }
        if [ "$daemon_mode" = signed-current ] && [ -n "$daemon_revision" ] && [ "$daemon_revision" != "$source_revision" ]; then
            _macos_app_service_reconcile code-intel-daemon "$HOME/.codeintel" true || return 1
            if [ "$MACOS_APP_SERVICE_RECOVERY_PENDING" = true ]; then
                _macos_app_service_reconcile code-intel-daemon "$HOME/.codeintel" false || return 1
            fi
        fi
        if [ "$cli_mode" = signed-current ] && [ "$cli_revision" = "$source_revision" ]; then
            _macos_app_compatibility_alias code-intel-cli "$HOME/.codeintel" "$TARGET_DIR" "$source_revision" || return 1
        fi
        if [ "$daemon_mode" = signed-current ] && [ "$daemon_revision" = "$source_revision" ]; then
            _macos_app_compatibility_alias code-intel-daemon "$HOME/.codeintel" "$TARGET_DIR" "$source_revision" || return 1
        fi
        if [ "$cli_mode" = signed-current ] && [ "$cli_revision" = "$source_revision" ] && [ "$daemon_mode" = signed-current ] && [ "$daemon_revision" = "$source_revision" ]; then
            _macos_app_service_reconcile code-intel-daemon "$HOME/.codeintel" || return 1
            return 0
        fi
    fi
    local build_target
    local host_target
    if [ "$cli_managed" = true ] || [ "$daemon_managed" = true ]; then
        _managed_cargo_target "$target_dir" "$source_revision" || return 1
        target_dir="$MANAGED_CARGO_TARGET"
        build_target="$target_dir/code-intel-build.$$.${RANDOM}"
        _managed_cargo_target "$build_target" "$source_revision" true true || return 1
        build_target="$MANAGED_CARGO_TARGET"
    else
        mkdir -p "$target_dir" || return 1
        build_target=$(mktemp -d "$target_dir/code-intel-build.XXXXXX") || return 1
    fi
    host_target=$(rustc -vV | awk '/^host: / {print $2}') || { rm -rf -- "$build_target"; return 1; }
    case "$host_target" in
        ""|*/*|*" "*)
        echo "ERROR: rustc host target is invalid" >&2
        rm -rf -- "$build_target"
        return 1
        ;;
    esac
    cleanup_target() { rm -rf -- "$build_target"; }
    echo "  Building code-intel binaries (cq, scipd)..."
    if ! ( cd "$SCRIPT_DIR/system/code-intel" && CARGO_TARGET_DIR="$build_target" cargo build --locked --release --package scipd --bin cq --bin scipd --target "$host_target" 2>&1 ); then
        echo "  ERROR: code-intel build failed; no companion publication occurred" >&2
        cleanup_target
        return 1
    fi
    local source_after source_status_after
    source_status_after=$(git -C "$SCRIPT_DIR" status --porcelain --untracked-files=all) || { cleanup_target; return 1; }
    source_after=$(git -C "$SCRIPT_DIR" rev-parse HEAD) || { cleanup_target; return 1; }
    if [ -n "$source_status_after" ] || [ "$source_after" != "$source_revision" ]; then
        echo "  ERROR: source checkout changed during code-intel build; refusing companion publication" >&2
        cleanup_target
        return 1
    fi
    local name ci_bin
    for name in cq scipd; do
        ci_bin="$build_target/$host_target/release/$name"
        [ -x "$ci_bin" ] || { echo "ERROR: missing exact code-intel artifact: $ci_bin" >&2; cleanup_target; return 1; }
        local product=code-intel-cli root="$HOME/.codeintel"
        [ "$name" = scipd ] && product=code-intel-daemon
        if [ "$cli_managed" = true ]; then
            local source_now source_status_now
            source_status_now=$(git -C "$SCRIPT_DIR" status --porcelain --untracked-files=all) || { cleanup_target; return 1; }
            source_now=$(git -C "$SCRIPT_DIR" rev-parse HEAD) || { cleanup_target; return 1; }
            if [ -n "$source_status_now" ] || [ "$source_now" != "$source_revision" ]; then
                echo "  ERROR: source checkout changed before publishing $name; refusing companion publication" >&2
                cleanup_target
                return 1
            fi
            if { [ "$name" = cq ] && { [ "$cli_mode" != signed-current ] || [ "$cli_revision" != "$source_revision" ]; }; } || { [ "$name" = scipd ] && { [ "$daemon_mode" != signed-current ] || [ "$daemon_revision" != "$source_revision" ]; }; }; then
                _macos_app_install "$product" "$root" "$ci_bin" "$version" "$source_revision" "$source_revision" || { cleanup_target; return 1; }
                _macos_app_compatibility_alias "$product" "$root" "$TARGET_DIR" "$source_revision" || { cleanup_target; return 1; }
            fi
        else
            mkdir -p "$TARGET_DIR/.hex/bin"
            cp "$ci_bin" "$TARGET_DIR/.hex/bin/$name"
            chmod +x "$TARGET_DIR/.hex/bin/$name"
        fi
    done
    if [ "$cli_managed" = true ]; then
        _macos_app_service_reconcile code-intel-daemon "$HOME/.codeintel" || { cleanup_target; return 1; }
    fi
    cleanup_target
}

_harness_download_prebuilt() {
    local arch os harness_url
    if ! _macos_app_prepare hex "$TARGET_DIR/.hex"; then
        return 1
    fi
    if [ "$MACOS_APP_MANAGED" = true ]; then
        echo "ERROR: managed macOS Hex prebuilt installation is unsupported until artifact provenance is verified" >&2
        return 1
    fi
    arch=$(uname -m)
    os=$(uname -s | tr '[:upper:]' '[:lower:]')
    harness_url="https://github.com/mrap/hex-foundation/releases/download/${HARNESS_VERSION}/hex-${os}-${arch}"
    echo "  Downloading hex from ${harness_url}..."
    curl -fSL "$harness_url" -o "$TARGET_DIR/.hex/bin/hex" && chmod +x "$TARGET_DIR/.hex/bin/hex"
    ln -sf hex "$TARGET_DIR/.hex/bin/hex-agent"
    # A sidecar from a PRIOR from-source run must not outlive the binary it
    # described: leaving it would let `hex upgrade` see installed_sha ==
    # source_sha and print "no rebuild needed" for a binary that is actually
    # this prebuilt artifact. Absent sidecar = loud WARN-skip instead (S6).
    rm -f "$TARGET_DIR/.hex/bin/hex.sha"
    # No prebuilt cq/scipd on releases — code-intel binaries require cargo.
    echo "  NOTE: cq/scipd (code-intel) skipped — install Rust and re-run to build them."
    # Deliberately NO hex.sha sidecar on this path: the binary came from a
    # release tarball, NOT $SCRIPT_DIR's checkout, so recording that source
    # HEAD would falsely assert "built from this source" and make `hex upgrade`
    # silently skip the freshness rebuild (installed_sha == source_sha match).
    # Leaving it absent makes upgrade WARN-skip freshness loudly instead (S6).
    echo "  ⚠️  WARN: prebuilt hex binary — no hex.sha recorded; hex upgrade cannot verify binary freshness" >&2
}

_harness_warn_missing() {
    echo ""
    echo "WARNING: hex binary could not be built or downloaded."
    echo "  Install Rust (https://rustup.rs) and re-run to build the hex binary."
    echo "  Core shell functionality (BOI, memory scripts) still works without it."
    echo ""
}

# Write the source checkout's git HEAD SHA next to the freshly built binary
# (.hex/bin/hex.sha) using an atomic tmp+rename, mirroring upgrade.rs. `hex
# upgrade` compares this sidecar against `git -C <source> rev-parse HEAD`;
# without it a fresh install has installed_sha=None and upgrade must WARN-skip
# the binary-freshness check forever. If the source git HEAD is unresolvable
# (source is not a git checkout, git failed), print a loud WARN mentioning
# hex.sha and continue — NEVER fail the install over the sidecar (S6). Only
# call this on the build-from-source path: a prebuilt binary did not come from
# $SCRIPT_DIR's checkout, so recording that SHA would falsely assert freshness.
# NOTE: this stamps THIS checkout's HEAD, while `hex upgrade` compares against
# a fresh pull of DEFAULT_REPO's default branch — installing from a non-default
# branch therefore triggers ONE extra rebuild on the next upgrade, after which
# the sidecar is rewritten from upgrade's own source dir (self-correcting).
write_hex_sha_sidecar() {
    local sha_file="$TARGET_DIR/.hex/bin/hex.sha"
    local src_sha=""
    src_sha=$(git -C "$SCRIPT_DIR" rev-parse HEAD 2>/dev/null || true)
    if [ -n "$src_sha" ]; then
        local sha_tmp="$sha_file.tmp"
        if printf '%s' "$src_sha" > "$sha_tmp" 2>/dev/null && mv -f "$sha_tmp" "$sha_file" 2>/dev/null; then
            echo "  hex.sha sidecar     ✓ (${src_sha:0:8})"
        else
            rm -f "$sha_tmp" 2>/dev/null || true
            echo "  ⚠️  WARN: could not write hex.sha sidecar — hex upgrade will not verify binary freshness" >&2
        fi
    else
        echo "  ⚠️  WARN: source git HEAD unresolvable — skipping hex.sha sidecar; hex upgrade will WARN-skip binary freshness checks" >&2
    fi
    return 0
}

if command -v cargo &>/dev/null; then
    _harness_build_from_source || {
        if [ "$MACOS_APP_MANAGED" = true ]; then
            echo "ERROR: managed macOS source build failed; refusing prebuilt or raw fallback" >&2
            exit 1
        fi
        echo "  Build failed — trying pre-built binary download..."
        if command -v curl &>/dev/null; then
            _harness_download_prebuilt || _harness_warn_missing
        else
            echo "  curl not found — skipping pre-built download"
            _harness_warn_missing
        fi
    }
elif command -v curl &>/dev/null; then
    echo "  cargo not found — trying pre-built binary download..."
    if ! _harness_download_prebuilt; then
        if [ "$MACOS_APP_MANAGED" = true ]; then
            exit 1
        fi
        _harness_warn_missing
    fi
else
    echo "  cargo and curl not found — skipping binary install"
    _harness_warn_missing
fi

# Copy SSE topic manifests
if [ -d "$SCRIPT_DIR/system/sse/topics" ]; then
    cp -R "$SCRIPT_DIR/system/sse/topics/"*.yaml "$TARGET_DIR/.hex/sse/topics/" 2>/dev/null || true
fi

# Copy CLI helpers
for helper in hex-asset hex-comment-respond.sh hex-sse-publish hex-sse-listen; do
    if [ -f "$SCRIPT_DIR/system/scripts/bin/$helper" ]; then
        cp "$SCRIPT_DIR/system/scripts/bin/$helper" "$TARGET_DIR/.hex/bin/$helper"
        chmod +x "$TARGET_DIR/.hex/bin/$helper"
    fi
done

if [ -x "$TARGET_DIR/.hex/bin/hex" ]; then
    if ! "$TARGET_DIR/.hex/bin/hex" version &>/dev/null; then
        echo "WARNING: hex binary installed but failed to execute. Re-run install to retry."
    else
        hex_ver=$("$TARGET_DIR/.hex/bin/hex" version 2>/dev/null || echo "unknown")
        echo "  hex binary          ✓ ($hex_ver)"
        # Verify symlink works
        if [ -L "$TARGET_DIR/.hex/bin/hex-agent" ]; then
            echo "  hex-agent symlink   ✓"
        else
            echo "  hex-agent symlink   ⚠ (creating...)"
            ln -sf hex "$TARGET_DIR/.hex/bin/hex-agent"
        fi
    fi
else
    echo "  hex binary          ⚠ (install Rust to enable agent fleet + server)"
fi

# ── Phase 8: Shell environment setup ─────────────────────────────

SHELL_RC=""
if [[ -n "${ZSH_VERSION:-}" ]] || [[ "$SHELL" == */zsh ]]; then
    SHELL_RC="$HOME/.zshrc"
elif [[ -n "${BASH_VERSION:-}" ]] || [[ "$SHELL" == */bash ]]; then
    SHELL_RC="$HOME/.bashrc"
fi

if [[ -n "$SHELL_RC" ]]; then
    NEEDS_WRITE=false
    if ! grep -q 'export HEX_DIR=' "$SHELL_RC" 2>/dev/null; then
        NEEDS_WRITE=true
    fi

    if $NEEDS_WRITE; then
        echo "Setting up shell environment in $SHELL_RC..."
        cat >> "$SHELL_RC" << RCEOF

# =====================
# Hex Agent
# =====================
export HEX_DIR="$TARGET_DIR"
export AGENT_DIR="\$HEX_DIR"  # deprecated alias — use HEX_DIR
export PATH="\$HEX_DIR/.hex/bin:\$PATH"
RCEOF
        echo "  HEX_DIR, AGENT_DIR (deprecated alias), PATH added to $SHELL_RC ✓"
        echo "  Run 'source $SHELL_RC' or restart your terminal to activate."
    else
        echo "  HEX_DIR already in $SHELL_RC ✓"
    fi

    # Shell completions — sourced from the binary so they always match the
    # installed version. Self-contained (no fpath/compinit ordering deps).
    if ! grep -q 'hex completions' "$SHELL_RC" 2>/dev/null; then
        if [[ "$SHELL_RC" == *.bashrc ]]; then COMP_SHELL="bash"; else COMP_SHELL="zsh"; fi
        cat >> "$SHELL_RC" << RCEOF

# hex shell completions
command -v hex >/dev/null 2>&1 && source <(hex completions $COMP_SHELL)
RCEOF
        echo "  hex completions ($COMP_SHELL) added to $SHELL_RC ✓"
    else
        echo "  hex completions already in $SHELL_RC ✓"
    fi
else
    echo ""
    echo "Add these to your shell rc file:"
    echo "  export HEX_DIR=\"$TARGET_DIR\""
    echo "  export AGENT_DIR=\"\$HEX_DIR\"  # deprecated alias — use HEX_DIR"
    echo "  export PATH=\"\$HEX_DIR/.hex/bin:\$PATH\""
fi

echo ""
echo "========================================="
echo " hex installed at $TARGET_DIR"
echo "========================================="
echo ""
echo "Start your first session:"
echo "  cd $TARGET_DIR && claude"
echo ""
echo "Your agent will walk you through setup."
