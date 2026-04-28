#!/usr/bin/env python3
"""hex-events — CLI for querying and debugging the event system.

Usage:
  hex-events status                  # Daemon running? Last poll? Recipe count?
  hex-events history [--since 1]     # Event timeline (hours)
  hex-events inspect <event-id>      # Full trace for one event
  hex-events recipes                 # List loaded recipes
  hex-events test <recipe-file>      # Dry-run a recipe with a mock event
  hex-events validate                # Static policy graph validation
  hex-events graph [--observed]      # Show event dependency graph
  hex-events trace <event-id>        # Policy evaluation trace for an event
  hex-events trace --policy <name> [--since <hours>]  # Policy trace over time
"""
import argparse
import hashlib
import json
import os
import subprocess
import sys
from datetime import datetime, timezone

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from db import EventsDB
from recipe import load_recipes

BASE_DIR = os.path.expanduser("~/.hex-events")
COMPILER_VERSION = "1.0.0"
DB_PATH = os.path.join(BASE_DIR, "events.db")
RECIPES_DIR = os.path.join(BASE_DIR, "recipes")
POLICIES_DIR = os.path.join(BASE_DIR, "policies")

def cmd_status(args):
    # Check if daemon is running
    result = subprocess.run(["pgrep", "-f", "hex_eventd"], capture_output=True, text=True)
    running = result.returncode == 0
    pid = result.stdout.strip().split("\n")[0] if running else None

    policies, _ = _load_all_policies()
    db = EventsDB(DB_PATH)
    unprocessed = len(db.get_unprocessed())
    total = len(db.history(limit=1000))
    db.close()

    print(f"Daemon:      {'running (pid {})'.format(pid) if running else 'NOT RUNNING'}")
    print(f"Policies:    {len(policies)} loaded")
    print(f"Unprocessed: {unprocessed} events")
    print(f"Total (7d):  {total} events")

def cmd_history(args):
    db = EventsDB(DB_PATH)
    events = db.history(limit=50, since_hours=args.since)
    event_ids = [e["id"] for e in events]
    rate_limited_map = db.get_rate_limited_by_event(event_ids)
    db.close()
    if not events:
        print("No events found.")
        return
    for e in reversed(events):  # oldest first
        eid = e["id"]
        if eid in rate_limited_map:
            marker = "⊘"
            policy = rate_limited_map[eid]
            suffix = f"(rate limited: {policy})"
        elif e["processed_at"]:
            marker = "✓"
            suffix = e["recipe"] or ""
        else:
            marker = "·"
            suffix = e["recipe"] or ""
        print(f"  {marker} [{eid:4d}] {e['created_at']}  {e['event_type']:30s}  {e['source']:15s}  {suffix}")

def _format_condition_detail(idx: int, detail: dict) -> str:
    """Format a single condition detail for inspect output."""
    field = detail.get("field", "?")
    op = detail.get("op", "?")
    expected = detail.get("expected", "?")
    passed = detail.get("passed")

    if passed == "not_evaluated":
        return f"    Condition {idx}: {field} {op} {expected} (not evaluated, short-circuited)"

    actual = detail.get("actual")
    marker = "✓" if passed else "✗"
    return f"    Condition {idx}: {field} {op} {expected} → actual: {actual} {marker}"


def cmd_inspect(args):
    db = EventsDB(DB_PATH)
    rows = db.conn.execute("SELECT * FROM events WHERE id = ?", (args.event_id,)).fetchall()
    if not rows:
        print(f"Event {args.event_id} not found.")
        db.close()
        return
    event = dict(rows[0])
    print(f"Event #{event['id']}: {event['event_type']}")
    print(f"  Source:    {event['source']}")
    print(f"  Created:   {event['created_at']}")
    print(f"  Processed: {event['processed_at'] or 'not yet'}")
    print(f"  Recipe:    {event['recipe'] or 'none matched'}")
    print(f"  Payload:   {event['payload']}")

    # Show policy evaluation details
    policy_evals = db.get_policy_evals(args.event_id)
    if policy_evals:
        print("  Policy Evaluations:")
        for eval_row in policy_evals:
            policy_name = eval_row["policy_name"]
            rule_name = eval_row["rule_name"]
            rate_limited = eval_row.get("rate_limited")
            action_taken = eval_row.get("action_taken")
            print(f"    Policy: {policy_name}")
            print(f"      Rule: {rule_name}")
            if rate_limited:
                print(f"      (rate limited)")
            else:
                cond_json = eval_row.get("condition_details")
                if cond_json:
                    try:
                        cond_details = json.loads(cond_json)
                        for i, detail in enumerate(cond_details, 1):
                            print(_format_condition_detail(i, detail))
                    except (json.JSONDecodeError, TypeError):
                        pass
                status = "success" if action_taken else "skipped"
                print(f"      Actions: {status}")

    logs = db.get_action_logs(args.event_id)
    if logs:
        print("  Action Log:")
        for log in logs:
            print(f"    [{log['status']}] {log['action_type']} — {log['action_detail'][:80]}")
            if log["error_message"]:
                print(f"           {log['error_message'][:100]}")
    db.close()

def _format_trace_row(event_type: str, row: dict, action_logs: list) -> str:
    """Format a single policy_eval_log row for the trace output."""
    rate_limited = row.get("rate_limited")
    conditions_passed = row.get("conditions_passed")
    action_taken = row.get("action_taken")
    policy_name = row["policy_name"]
    rule_name = row["rule_name"]

    if rate_limited:
        marker = "⊘"
    elif conditions_passed:
        marker = "✓"
    else:
        marker = "✗"

    lines = [f"  {marker} {policy_name}", f"    Rule: {rule_name}"]

    if rate_limited:
        rl_entry = next(
            (l for l in action_logs
             if l.get("recipe") == policy_name and l.get("action_type") == "rate_limited"),
            None,
        )
        lines.append(f"    Trigger: matched ({event_type})")
        if rl_entry:
            try:
                d = json.loads(rl_entry["action_detail"])
                fires = d.get("fires_in_window", "?")
                max_fires = d.get("max_fires", "?")
                window = d.get("window", "?")
                lines.append(f"    Rate limited: yes ({fires}/{max_fires} fires in {window} window)")
            except (json.JSONDecodeError, TypeError):
                lines.append("    Rate limited: yes")
        else:
            lines.append("    Rate limited: yes")
    else:
        lines.append(f"    Trigger: matched ({event_type})")
        cond_json = row.get("condition_details")
        if cond_json:
            try:
                cond_details = json.loads(cond_json)
                if conditions_passed:
                    parts = []
                    for c in cond_details:
                        if c.get("passed") and c.get("passed") != "not_evaluated":
                            field = c.get("field", "?")
                            op = c.get("op", "?")
                            expected = c.get("expected", "?")
                            actual = c.get("actual")
                            parts.append(f"{field} {op} {expected} → actual: {actual}")
                    cond_str = "; ".join(parts) if parts else "all passed"
                    lines.append(f"    Conditions: passed ({cond_str})")
                else:
                    failed = next(
                        (c for c in cond_details if c.get("passed") is False),
                        None,
                    )
                    if failed:
                        field = failed.get("field", "?")
                        op = failed.get("op", "?")
                        expected = failed.get("expected", "?")
                        actual = failed.get("actual")
                        lines.append(
                            f"    Conditions: failed ({field} {op} {expected} → actual: {actual})"
                        )
                    else:
                        lines.append("    Conditions: failed")
            except (json.JSONDecodeError, TypeError):
                status = "passed" if conditions_passed else "failed"
                lines.append(f"    Conditions: {status}")
        elif conditions_passed is not None:
            status = "passed" if conditions_passed else "failed"
            lines.append(f"    Conditions: {status}")

        if action_taken:
            relevant = [
                l for l in action_logs
                if l.get("recipe") == policy_name or l.get("recipe") == rule_name
            ]
            if relevant:
                for al in relevant:
                    atype = al["action_type"]
                    status = al["status"]
                    err = (al.get("error_message") or "")[:80]
                    if err:
                        lines.append(f"    Actions: {atype} → {status} ({err})")
                    else:
                        lines.append(f"    Actions: {atype} → {status}")
            else:
                lines.append("    Actions: taken")

    return "\n".join(lines)


def cmd_trace(args):
    db = EventsDB(DB_PATH)

    # Mode: --policy [--since] without event_id — show policy trace over time
    if args.policy and args.event_id is None:
        since = args.since or 24
        evals = db.get_policy_evals_since(args.policy, since)
        db.close()
        if not evals:
            print(f"No evaluations found for policy '{args.policy}' in the last {since}h.")
            return
        print(f"Policy: {args.policy} (last {since}h)\n")
        for row in evals:
            event_id = row["event_id"]
            event_type = row.get("event_type", "?")
            event_ts = row.get("event_created_at", "?")
            rate_limited = row.get("rate_limited")
            conditions_passed = row.get("conditions_passed")
            if rate_limited:
                marker = "⊘"
            elif conditions_passed:
                marker = "✓"
            else:
                marker = "✗"
            print(f"  {marker} Event #{event_id}: {event_type} ({event_ts})")
            print(f"    Rule: {row['rule_name']}")
            if rate_limited:
                print("    Rate limited: yes")
            elif conditions_passed is not None:
                status = "passed" if conditions_passed else "failed"
                print(f"    Conditions: {status}")
            print()
        return

    # Mode: <event-id> trace
    if args.event_id is None:
        print("Usage: hex-events trace <event-id> [--policy <name>]")
        print("       hex-events trace --policy <name> [--since <hours>]")
        db.close()
        return

    rows = db.conn.execute("SELECT * FROM events WHERE id = ?", (args.event_id,)).fetchall()
    if not rows:
        print(f"Event {args.event_id} not found.")
        db.close()
        return

    event = dict(rows[0])
    event_type = event["event_type"]
    created_at = event["created_at"]
    print(f"Event #{event['id']}: {event_type} ({created_at})\n")

    policy_evals = db.get_policy_evals(args.event_id, policy_name=args.policy)
    action_logs = db.get_action_logs(args.event_id)
    db.close()

    print("Policy evaluations:\n")

    # Group by policy name
    eval_by_policy: dict[str, list] = {}
    for row in policy_evals:
        pname = row["policy_name"]
        eval_by_policy.setdefault(pname, []).append(row)

    shown: set[str] = set()
    for pname, evals in eval_by_policy.items():
        for row in evals:
            print(_format_trace_row(event_type, row, action_logs))
            print()
        shown.add(pname)

    # Show policies with no log entries (trigger didn't match) — only without a policy filter
    if not args.policy:
        try:
            policies, _ = _load_all_policies()
        except Exception:
            policies = []
        for policy in policies:
            if policy.name not in shown:
                for rule in policy.rules:
                    print(f"  ✗ {policy.name}")
                    print(f"    Rule: {rule.name}")
                    print(
                        f"    Trigger: no match (expected: {rule.trigger_event}, got: {event_type})"
                    )
                    print()

    if not policy_evals and not args.policy:
        try:
            policies, _ = _load_all_policies()
        except Exception:
            policies = []
        if not policies:
            print("  (no policy evaluations logged for this event)")


def _parse_etime(etime_str: str) -> str:
    """Convert ps etime ([[DD-]HH:]MM:SS) to human-readable uptime string."""
    s = etime_str.strip()
    days = 0
    if "-" in s:
        d, s = s.split("-", 1)
        days = int(d)
    parts = s.split(":")
    if len(parts) == 3:
        hours = int(parts[0]) + days * 24
        mins = int(parts[1])
    elif len(parts) == 2:
        hours = days * 24
        mins = int(parts[0])
    else:
        return etime_str.strip()
    if hours > 0:
        return f"{hours}h {mins}m"
    return f"{mins}m"


def _last_daemon_activity(log_file: str) -> str:
    """Return human-readable time since last daemon log entry."""
    if not os.path.exists(log_file):
        return "unknown"
    try:
        with open(log_file, "rb") as f:
            f.seek(0, 2)
            size = f.tell()
            chunk = min(size, 50000)
            f.seek(-chunk, 2)
            tail = f.read().decode("utf-8", errors="replace")
        for line in reversed(tail.splitlines()):
            line = line.strip()
            if not line:
                continue
            # Log lines start with datetime: "2026-03-19 17:00:01,123 ..."
            parts = line.split()
            if len(parts) >= 2:
                try:
                    ts = datetime.strptime(parts[0] + " " + parts[1], "%Y-%m-%d %H:%M:%S,%f")
                    delta = datetime.utcnow() - ts
                    total_secs = int(delta.total_seconds())
                    if total_secs < 60:
                        return f"{total_secs}s ago"
                    mins = total_secs // 60
                    if mins < 60:
                        return f"{mins}m ago"
                    return f"{mins // 60}h {mins % 60}m ago"
                except ValueError:
                    continue
    except Exception:
        pass
    return "unknown"


def cmd_telemetry(args):
    db = EventsDB(DB_PATH)

    events_processed = db.conn.execute(
        "SELECT COUNT(*) as cnt FROM events WHERE created_at >= datetime('now', '-24 hours')"
    ).fetchone()["cnt"]

    actions_fired = db.conn.execute(
        "SELECT COUNT(*) as cnt FROM action_log al "
        "JOIN events e ON e.id = al.event_id "
        "WHERE e.created_at >= datetime('now', '-24 hours') "
        "AND al.action_type != 'rate_limited' "
        "AND al.status NOT IN ('suppressed', 'error') "
        "AND al.status NOT LIKE 'retry_%'"
    ).fetchone()["cnt"]

    actions_failed = db.conn.execute(
        "SELECT COUNT(*) as cnt FROM action_log al "
        "JOIN events e ON e.id = al.event_id "
        "WHERE e.created_at >= datetime('now', '-24 hours') "
        "AND al.status = 'error'"
    ).fetchone()["cnt"]

    rate_limits_hit = db.conn.execute(
        "SELECT COUNT(*) as cnt FROM action_log al "
        "JOIN events e ON e.id = al.event_id "
        "WHERE e.created_at >= datetime('now', '-24 hours') "
        "AND al.action_type = 'rate_limited'"
    ).fetchone()["cnt"]

    policy_violations = db.conn.execute(
        "SELECT COUNT(*) as cnt FROM events "
        "WHERE created_at >= datetime('now', '-24 hours') "
        "AND event_type LIKE '%violation%'"
    ).fetchone()["cnt"]

    top_policies = db.conn.execute(
        "SELECT pel.policy_name, COUNT(*) as fires "
        "FROM policy_eval_log pel "
        "JOIN events e ON e.id = pel.event_id "
        "WHERE e.created_at >= datetime('now', '-24 hours') "
        "AND pel.action_taken = 1 "
        "GROUP BY pel.policy_name ORDER BY fires DESC LIMIT 5"
    ).fetchall()

    errors = db.conn.execute(
        "SELECT al.recipe, al.action_type, COUNT(*) as cnt "
        "FROM action_log al "
        "JOIN events e ON e.id = al.event_id "
        "WHERE e.created_at >= datetime('now', '-24 hours') "
        "AND al.status = 'error' "
        "GROUP BY al.recipe, al.action_type ORDER BY cnt DESC LIMIT 10"
    ).fetchall()

    db.close()

    if getattr(args, "json", False):
        print(json.dumps({
            "events_processed": events_processed,
            "actions_fired": actions_fired,
            "actions_failed": actions_failed,
            "rate_limits_hit": rate_limits_hit,
            "policy_violations": policy_violations,
        }))
        return

    # Daemon info
    result = subprocess.run(["pgrep", "-f", "hex_eventd"], capture_output=True, text=True)
    daemon_running = result.returncode == 0
    pid = result.stdout.strip().split("\n")[0] if daemon_running else None

    uptime_str = "not running"
    if daemon_running and pid:
        try:
            ps = subprocess.run(
                ["ps", "-o", "etime=", "-p", pid], capture_output=True, text=True
            )
            if ps.returncode == 0 and ps.stdout.strip():
                uptime_str = _parse_etime(ps.stdout)
        except Exception:
            uptime_str = "unknown"

    log_file = os.path.join(BASE_DIR, "daemon.log")
    last_heartbeat_str = _last_daemon_activity(log_file)

    log_size_str = "N/A"
    rotations = 0
    if os.path.exists(log_file):
        try:
            size_bytes = os.path.getsize(log_file)
            for i in range(1, 10):
                if os.path.exists(log_file + f".{i}"):
                    rotations += 1
                else:
                    break
            if size_bytes >= 1024 * 1024:
                log_size_str = f"{size_bytes / (1024 * 1024):.1f} MB"
            else:
                log_size_str = f"{size_bytes / 1024:.1f} KB"
        except Exception:
            pass

    print("hex-events Telemetry (last 24h)")
    print("────────────────────────────")
    print(f"Events processed:     {events_processed}")
    print(f"Actions fired:        {actions_fired}")
    print(f"Actions failed:       {actions_failed}")
    print(f"Rate limits hit:      {rate_limits_hit}")
    print(f"Policy violations:    {policy_violations}")

    if top_policies:
        print()
        print("Top policies:")
        for row in top_policies:
            print(f"  {row['policy_name']:<30s}  {row['fires']} fires")

    if errors:
        print()
        print("Errors:")
        for row in errors:
            print(f"  {row['recipe']}: {row['cnt']} failures ({row['action_type']} action)")

    print()
    print("Daemon:")
    print(f"  Uptime: {uptime_str}")
    print(f"  Last heartbeat: {last_heartbeat_str}")
    if os.path.exists(log_file):
        print(f"  Log size: {log_size_str} ({rotations} rotations)")


def cmd_recipes(args):
    recipes = load_recipes(RECIPES_DIR)
    if not recipes:
        print("No recipes loaded.")
        return
    for r in recipes:
        print(f"  {r.name:25s}  trigger={r.trigger_event:25s}  actions={len(r.actions)}  conditions={len(r.conditions)}")

def cmd_test(args):
    from recipe import Recipe
    import yaml
    try:
        with open(args.recipe_file) as f:
            data = yaml.safe_load(f)
        recipe = Recipe.from_dict(data, source_file=args.recipe_file)
        print(f"Recipe: {recipe.name}")
        print(f"  Trigger: {recipe.trigger_event}")
        print(f"  Conditions: {len(recipe.conditions)}")
        print(f"  Actions: {len(recipe.actions)}")
        print(f"  [DRY RUN] Would fire {len(recipe.actions)} action(s) on matching event.")
    except FileNotFoundError:
        print(f"Recipe file not found: {args.recipe_file}")
        sys.exit(1)
    except yaml.YAMLError as e:
        print(f"Invalid YAML in {args.recipe_file}: {e}")
        sys.exit(1)
    except (KeyError, TypeError) as e:
        print(f"Invalid recipe structure: {e}")
        sys.exit(1)

def _load_all_policies():
    """Load policies from policies/ dir (preferred) or recipes/ dir (fallback)."""
    from policy import load_policies
    policies_dir = POLICIES_DIR if os.path.isdir(POLICIES_DIR) else RECIPES_DIR
    if not os.path.isdir(policies_dir):
        return [], policies_dir
    try:
        return load_policies(policies_dir), policies_dir
    except Exception as e:
        print(f"Warning: failed to load policies from {policies_dir}: {e}", file=sys.stderr)
        return [], policies_dir


def cmd_validate(args):
    from policy_validator import validate_policy_file

    # Determine which files to validate
    if hasattr(args, "file") and args.file:
        if os.path.isdir(args.file):
            files = sorted(
                os.path.join(args.file, f)
                for f in os.listdir(args.file)
                if f.endswith(".yaml") or f.endswith(".yml")
            )
        else:
            files = [args.file]
    else:
        policies_dir = POLICIES_DIR if os.path.isdir(POLICIES_DIR) else RECIPES_DIR
        if not os.path.isdir(policies_dir):
            print("No policies directory found.")
            sys.exit(0)
        files = sorted(
            os.path.join(policies_dir, f)
            for f in os.listdir(policies_dir)
            if f.endswith(".yaml") or f.endswith(".yml")
        )

    valid_count = 0
    invalid_count = 0

    for filepath in files:
        errors = validate_policy_file(filepath)
        try:
            import yaml
            with open(filepath) as f:
                policy = yaml.safe_load(f)
            rule_count = len(policy.get("rules", [])) if isinstance(policy, dict) else 0
        except Exception:
            rule_count = 0

        display_path = os.path.relpath(filepath) if os.path.isabs(filepath) else filepath

        if errors:
            invalid_count += 1
            print(f"{display_path}: ERROR")
            for err in errors:
                # Strip leading filename from error message for cleaner output
                msg = err
                if msg.startswith(filepath + ":"):
                    msg = msg[len(filepath) + 1:].strip()
                elif msg.startswith(filepath + " "):
                    msg = msg[len(filepath):].strip()
                print(f"  - {msg}")
        else:
            valid_count += 1
            print(f"{display_path}: OK ({rule_count} rules)")

    print()
    print(f"Summary: {valid_count} valid, {invalid_count} invalid")

    # --- Contract verification: dead triggers + orphan events ---
    _run_contract_check(files)

    if invalid_count > 0:
        sys.exit(1)


def _run_contract_check(policy_paths: list) -> None:
    """Run cross-event contract verification and print results."""
    try:
        from validators import contract_validator
    except ImportError:
        return

    scripts_dirs = [
        os.path.expanduser("~/.boi/src"),
        os.path.expanduser("~/mrap-hex/.hex/scripts"),
        os.path.join(BASE_DIR, "scripts") if os.path.isdir(os.path.join(BASE_DIR, "scripts")) else "",
    ]
    scripts_dirs = [d for d in scripts_dirs if d and os.path.isdir(d)]

    issues = contract_validator.validate_corpus(policy_paths, scripts_dirs=scripts_dirs)

    errors = [i for i in issues if i["severity"] == "error"]
    warnings = [i for i in issues if i["severity"] == "warning"]

    print()
    print(f"contract check: {len(policy_paths)} policies · {len(errors)} dead trigger(s) · {len(warnings)} orphan event(s)")

    for issue in sorted(issues, key=lambda i: (i["severity"], i["message"])):
        severity_label = "ERROR" if issue["severity"] == "error" else "WARNING"
        print(f"  [{severity_label}] {issue['message']}")


def cmd_graph(args):
    from validator import (
        build_static_graph, load_adapter_events,
        get_observed_events, compare_graphs,
    )

    policies, _policies_dir = _load_all_policies()
    adapter_events = load_adapter_events()
    graph = build_static_graph(policies, adapter_events=adapter_events)

    if args.observed:
        db = EventsDB(DB_PATH)
        observed = get_observed_events(db, days=7)
        db.close()

        print("=== Observed Event Graph (last 7 days) ===")
        print()
        if not observed["event_counts"]:
            print("No events observed in last 7 days.")
            return

        for evt, count in sorted(observed["event_counts"].items(),
                                  key=lambda x: -x[1]):
            consumers = observed["policy_triggers"].get(evt, [])
            consumer_str = ""
            if consumers:
                names = ", ".join(f"{p['policy']}x{p['count']}" for p in consumers)
                consumer_str = f"  -> [{names}]"
            print(f"  {evt:40s} {count:5d} events{consumer_str}")

        print()
        cmp = compare_graphs(graph, observed)
        if cmp["in_static_only"]:
            print(f"In static only (never observed): {', '.join(cmp['in_static_only'])}")
        if cmp["in_observed_only"]:
            print(f"In observed only (not in static): {', '.join(cmp['in_observed_only'])}")
        if cmp["in_both"]:
            print(f"In both: {', '.join(cmp['in_both'])}")

    else:
        print("=== Static Event Dependency Graph ===")
        print()
        all_events = sorted(
            set(graph["provided_by"].keys()) | set(graph["required_by"].keys())
        )
        if not all_events:
            print("No events in static graph.")
            return

        for evt in all_events:
            providers = graph["provided_by"].get(evt, [])
            consumers = graph["required_by"].get(evt, [])
            prov_str = ", ".join(providers) if providers else "(external)"
            cons_str = ", ".join(consumers) if consumers else "(terminal)"
            print(f"  {evt}")
            print(f"    provided by: {prov_str}")
            print(f"    consumed by: {cons_str}")
        print()
        print(f"Total: {len(all_events)} events, {len(policies)} policies, "
              f"{len(adapter_events)} adapter events")


def _get_workflow_dirs():
    """Return list of (dir_name, dir_path) for all subdirectories in policies/."""
    if not os.path.isdir(POLICIES_DIR):
        return []
    result = []
    for entry in sorted(os.listdir(POLICIES_DIR)):
        entry_path = os.path.join(POLICIES_DIR, entry)
        if os.path.isdir(entry_path):
            result.append((entry, entry_path))
    return result


def _load_workflow_info(dir_name, dir_path):
    """Load workflow metadata from a directory."""
    import yaml
    config_path = os.path.join(dir_path, "_config.yaml")
    config = {}
    if os.path.isfile(config_path):
        try:
            with open(config_path) as f:
                config = yaml.safe_load(f) or {}
        except Exception:
            pass

    name = config.get("name", dir_name)
    description = config.get("description", "")
    enabled = config.get("enabled", True)
    disabled_file = os.path.exists(os.path.join(dir_path, ".disabled"))
    is_enabled = enabled and not disabled_file
    shared_config = config.get("config", {})

    # Count policy files
    policy_count = 0
    policy_names = []
    for fname in sorted(os.listdir(dir_path)):
        if fname.startswith("_") or fname == ".disabled":
            continue
        if fname.endswith((".yaml", ".yml")):
            policy_count += 1
            policy_names.append(fname.replace(".yaml", "").replace(".yml", ""))

    return {
        "name": name,
        "dir_name": dir_name,
        "dir_path": dir_path,
        "description": description,
        "enabled": is_enabled,
        "disabled_file": disabled_file,
        "config_enabled": enabled,
        "shared_config": shared_config,
        "policy_count": policy_count,
        "policy_names": policy_names,
    }


def cmd_workflows(args):
    """List all workflows with status."""
    workflow_dirs = _get_workflow_dirs()
    if not workflow_dirs:
        print("No workflows found. Create one by making a subdirectory in policies/.")
        return

    db = EventsDB(DB_PATH)

    print("Workflows:")
    for dir_name, dir_path in workflow_dirs:
        info = _load_workflow_info(dir_name, dir_path)
        status = "enabled" if info["enabled"] else "disabled"

        # Get 24h event count for policies in this workflow
        event_count = 0
        last_eval = None
        if info["policy_names"]:
            placeholders = ",".join("?" * len(info["policy_names"]))
            row = db.conn.execute(
                f"SELECT COUNT(*) as cnt, MAX(evaluated_at) as last_at "
                f"FROM policy_eval_log WHERE policy_name IN ({placeholders}) "
                f"AND evaluated_at >= datetime('now', '-24 hours')",
                info["policy_names"],
            ).fetchone()
            event_count = row["cnt"] or 0
            last_eval = row["last_at"]

        last_str = ""
        if last_eval:
            try:
                ts = datetime.strptime(last_eval, "%Y-%m-%dT%H:%M:%S.%f")
            except ValueError:
                try:
                    ts = datetime.strptime(last_eval, "%Y-%m-%dT%H:%M:%S")
                except ValueError:
                    ts = None
            if ts:
                delta = datetime.utcnow() - ts
                secs = int(delta.total_seconds())
                if secs < 60:
                    last_str = f"last: {secs}s ago"
                elif secs < 3600:
                    last_str = f"last: {secs // 60}m ago"
                else:
                    last_str = f"last: {secs // 3600}h ago"

        parts = [
            f"  {info['name']:<22s}",
            f"{info['policy_count']} policies",
            f"{status:>8s}",
        ]
        if last_str:
            parts.append(f"{last_str:>14s}")
        if event_count:
            parts.append(f"24h: {event_count} evals")
        print("   ".join(parts))

    # List standalone policies
    standalone = []
    for entry in sorted(os.listdir(POLICIES_DIR)):
        entry_path = os.path.join(POLICIES_DIR, entry)
        if os.path.isfile(entry_path) and entry.endswith((".yaml", ".yml")):
            standalone.append(entry.replace(".yaml", "").replace(".yml", ""))
    if standalone:
        print(f"\nStandalone: {len(standalone)} policies ({', '.join(standalone)})")

    db.close()


def cmd_workflow(args):
    """Show workflow details, or enable/disable a workflow."""
    workflow_name = args.name
    action = args.action

    # Find the workflow directory
    workflow_dir = os.path.join(POLICIES_DIR, workflow_name)
    if not os.path.isdir(workflow_dir):
        # Try matching by config name
        found = False
        for dir_name, dir_path in _get_workflow_dirs():
            info = _load_workflow_info(dir_name, dir_path)
            if info["name"] == workflow_name:
                workflow_dir = dir_path
                workflow_name = dir_name
                found = True
                break
        if not found:
            print(f"Workflow '{args.name}' not found.")
            return

    info = _load_workflow_info(workflow_name, workflow_dir)

    if action == "enable":
        disabled_path = os.path.join(workflow_dir, ".disabled")
        if os.path.exists(disabled_path):
            os.remove(disabled_path)
        print(f"Workflow {info['name']} enabled ({info['policy_count']} policies active)")
        return

    if action == "disable":
        disabled_path = os.path.join(workflow_dir, ".disabled")
        with open(disabled_path, "w") as f:
            f.write("")
        print(f"Workflow {info['name']} disabled ({info['policy_count']} policies paused)")
        return

    if action == "status":
        db = EventsDB(DB_PATH)
        print(f"Workflow: {info['name']}")
        print(f"  Status: {'enabled' if info['enabled'] else 'disabled'}")
        print(f"  Policies: {info['policy_count']}")
        if info["shared_config"]:
            print(f"  Config:")
            for k, v in info["shared_config"].items():
                print(f"    {k}: {v}")
        print()
        print("  Policy evaluations (24h):")
        for pname in info["policy_names"]:
            row = db.conn.execute(
                "SELECT COUNT(*) as cnt, MAX(evaluated_at) as last_at "
                "FROM policy_eval_log WHERE policy_name = ? "
                "AND evaluated_at >= datetime('now', '-24 hours')",
                (pname,),
            ).fetchone()
            cnt = row["cnt"] or 0
            last = row["last_at"] or "never"
            print(f"    {pname:<35s}  {cnt:>4d} evals   last: {last}")
        db.close()
        return

    # Default: show workflow details
    print(f"Workflow: {info['name']}")
    if info["description"]:
        print(f"  Description: {info['description']}")
    print(f"  Status:      {'enabled' if info['enabled'] else 'disabled'}")
    print(f"  Directory:   {workflow_dir}")
    print(f"  Policies:    {info['policy_count']}")
    if info["shared_config"]:
        print(f"  Config:")
        for k, v in info["shared_config"].items():
            print(f"    {k}: {v}")
    print()
    print("  Policies:")
    for pname in info["policy_names"]:
        print(f"    {pname}")


def _build_event_catalog(policies_dir: str = POLICIES_DIR,
                          scheduler_config: str | None = None) -> dict:
    """Build {event: {producers: [...], consumers: [...]}} from scheduler + policies."""
    import yaml

    if scheduler_config is None:
        scheduler_config = os.path.join(BASE_DIR, "adapters", "scheduler.yaml")

    catalog: dict = {}

    def _entry(event: str) -> dict:
        if event not in catalog:
            catalog[event] = {"producers": [], "consumers": []}
        return catalog[event]

    # Scheduler producers
    if os.path.exists(scheduler_config):
        with open(scheduler_config) as f:
            data = yaml.safe_load(f) or {}
        for sched in data.get("schedules", []):
            evt = sched.get("event")
            if evt:
                _entry(evt)["producers"].append({"kind": "scheduler", "name": sched.get("name", "")})

    # Policy consumers + producers
    if os.path.isdir(policies_dir):
        for fname in sorted(os.listdir(policies_dir)):
            if not (fname.endswith(".yaml") or fname.endswith(".yml")):
                continue
            fpath = os.path.join(policies_dir, fname)
            try:
                with open(fpath) as f:
                    pol = yaml.safe_load(f) or {}
            except Exception:
                continue
            if not isinstance(pol, dict):
                continue
            policy_name = pol.get("name", fname)

            # provides.events — policy-level producers
            for evt in (pol.get("provides") or {}).get("events", []):
                _entry(evt)  # ensure entry exists

            # requires.events — consumers
            for evt in (pol.get("requires") or {}).get("events", []):
                _entry(evt)["consumers"].append({"policy": policy_name})

            for rule in pol.get("rules", []):
                if not isinstance(rule, dict):
                    continue
                rule_name = rule.get("name", "")

                # rule trigger event — consumer
                trigger_evt = (rule.get("trigger") or {}).get("event")
                if trigger_evt:
                    entry = _entry(trigger_evt)
                    already = any(
                        c.get("policy") == policy_name and c.get("rule") == rule_name
                        for c in entry["consumers"]
                    )
                    if not already:
                        entry["consumers"].append({"policy": policy_name, "rule": rule_name})

                # on_success / on_failure emit actions — producers
                for hook in ("on_success", "on_failure"):
                    for action in rule.get(hook, []):
                        if isinstance(action, dict) and action.get("type") == "emit":
                            emitted = action.get("event")
                            if emitted:
                                _entry(emitted)["producers"].append(
                                    {"kind": "policy", "name": policy_name, "rule": rule_name}
                                )

                # per-action on_success/on_failure
                for action in rule.get("actions", []):
                    if not isinstance(action, dict):
                        continue
                    for hook in ("on_success", "on_failure"):
                        for sub_action in action.get(hook, []):
                            if isinstance(sub_action, dict) and sub_action.get("type") == "emit":
                                emitted = sub_action.get("event")
                                if emitted:
                                    _entry(emitted)["producers"].append(
                                        {"kind": "policy", "name": policy_name, "rule": rule_name}
                                    )

    return catalog


def cmd_list_events(args):
    catalog = _build_event_catalog()
    fmt = getattr(args, "format", None)

    if fmt == "json":
        print(json.dumps({"events": catalog}, indent=2))
        return

    # Human-readable table
    all_events = sorted(catalog.keys())
    if not all_events:
        print("No events found.")
        return

    for evt in all_events:
        entry = catalog[evt]
        producers = entry["producers"]
        consumers = entry["consumers"]
        orphan_consumer = len(consumers) > 0 and len(producers) == 0
        orphan_producer = len(producers) > 0 and len(consumers) == 0
        flag = " ⚠️" if orphan_consumer else (" ℹ️" if orphan_producer else "")
        print(f"{evt}{flag}")
        if producers:
            for p in producers:
                if p.get("kind") == "scheduler":
                    print(f"  ← scheduler:{p['name']}")
                else:
                    print(f"  ← policy:{p['name']} (rule:{p.get('rule', '')})")
        else:
            print("  ← (no producer)")
        if consumers:
            for c in consumers:
                rule = c.get("rule", "")
                print(f"  → {c['policy']}" + (f" rule:{rule}" if rule else ""))
        else:
            print("  → (no consumer)")


_SCHEMA_CODES = {
    "YAML_PARSE_ERROR", "FILE_READ_ERROR", "NOT_A_DICT", "SCHEMA_FLAT_FORM",
    "MISSING_NAME", "MISSING_RULES", "RULE_NOT_DICT", "RULE_MISSING_NAME",
    "RULE_MISSING_TRIGGER", "RULE_TRIGGER_NO_EVENT",
    "NO_ACTIONS", "ACTION_NOT_DICT", "ACTION_MISSING_TYPE",
}
_PRODUCER_CODES = {"EVENT_NO_PRODUCER", "EVENT_MULTIPLE_PRODUCERS"}
_DEADCODE_CODES = {
    "DUPLICATE_RULE_NAME", "DUPLICATE_POLICY_NAME",
    "UNKNOWN_ACTION_TYPE", "NO_ACTIONS", "RATE_LIMIT_CADENCE_MISMATCH",
}


def _resolve_check_paths(path: str) -> list[str]:
    """Resolve a path argument to a list of YAML policy files."""
    if os.path.isfile(path):
        return [path]
    if os.path.isdir(path):
        events_dir = os.path.join(path, "events")
        search_dir = events_dir if os.path.isdir(events_dir) else path
        return sorted(
            os.path.join(search_dir, f)
            for f in os.listdir(search_dir)
            if f.endswith(".yaml") or f.endswith(".yml")
        )
    return []


def cmd_check(args):
    from validators import schema as schema_v
    from validators import producer_check as producer_v
    from validators import deadcode as deadcode_v

    permissive = getattr(args, "permissive", False)
    fmt = getattr(args, "format", None)

    if getattr(args, "all", False):
        paths = (
            sorted(
                os.path.join(POLICIES_DIR, f)
                for f in os.listdir(POLICIES_DIR)
                if f.endswith(".yaml") or f.endswith(".yml")
            )
            if os.path.isdir(POLICIES_DIR)
            else []
        )
    else:
        paths = _resolve_check_paths(args.path)

    if not paths:
        src = getattr(args, "path", "--all")
        print(f"No YAML files found at: {src}", file=sys.stderr)
        sys.exit(1)

    catalog = _build_event_catalog()

    file_results = []
    for p in paths:
        issues: list[dict] = []
        issues += schema_v.validate(p)
        issues += producer_v.validate(p, catalog)
        issues += deadcode_v.validate(p)
        file_results.append({"path": p, "issues": issues})

    # Corpus-level checks (e.g. duplicate policy names across files)
    corpus_issues = deadcode_v.validate_corpus(paths)
    corpus_by_file: dict[str, list] = {}
    for issue in corpus_issues:
        corpus_by_file.setdefault(issue["location"]["file"], []).append(issue)
    for fr in file_results:
        fr["issues"] += corpus_by_file.get(fr["path"], [])

    total_errors = sum(1 for fr in file_results for i in fr["issues"] if i["severity"] == "error")
    total_warnings = sum(1 for fr in file_results for i in fr["issues"] if i["severity"] == "warning")

    # Split compiled vs legacy when --all is used
    all_mode = getattr(args, "all", False)

    def _is_compiled(path: str) -> bool:
        try:
            with open(path) as fh:
                return fh.readline().startswith("# generated_from:")
        except OSError:
            return False

    if all_mode:
        compiled_results = [fr for fr in file_results if _is_compiled(fr["path"])]
        legacy_results = [fr for fr in file_results if not _is_compiled(fr["path"])]
        compiled_errors = sum(1 for fr in compiled_results for i in fr["issues"] if i["severity"] == "error")
        compiled_warnings = sum(1 for fr in compiled_results for i in fr["issues"] if i["severity"] == "warning")
        legacy_errors = sum(1 for fr in legacy_results for i in fr["issues"] if i["severity"] == "error")
        legacy_warnings = sum(1 for fr in legacy_results for i in fr["issues"] if i["severity"] == "warning")
    else:
        compiled_results = legacy_results = []
        compiled_errors = compiled_warnings = legacy_errors = legacy_warnings = 0

    if fmt == "json":
        summary: dict = {
            "files": len(file_results),
            "errors": total_errors,
            "warnings": total_warnings,
        }
        if all_mode:
            summary["compiled"] = {
                "files": len(compiled_results),
                "errors": compiled_errors,
                "warnings": compiled_warnings,
            }
            summary["legacy"] = {
                "files": len(legacy_results),
                "errors": legacy_errors,
                "warnings": legacy_warnings,
            }
        print(json.dumps({
            "files": file_results,
            "summary": summary,
        }, indent=2))
    else:
        for fr in file_results:
            p = fr["path"]
            issues = fr["issues"]
            print(p)

            schema_issues = [i for i in issues if i["code"] in _SCHEMA_CODES]
            producer_issues = [i for i in issues if i["code"] in _PRODUCER_CODES]
            deadcode_issues = [i for i in issues if i["code"] in _DEADCODE_CODES]

            for validator_name, v_issues in [
                ("schema", schema_issues),
                ("producer-check", producer_issues),
                ("dead-code", deadcode_issues),
            ]:
                if not v_issues:
                    print(f"  ✓ {validator_name}")
                else:
                    for iss in v_issues:
                        marker = "✗" if iss["severity"] == "error" else "⚠"
                        print(f"  {marker} {iss['code']}: {iss['message']}")

        n = len(file_results)
        err_word = "error" if total_errors == 1 else "errors"
        warn_word = "warning" if total_warnings == 1 else "warnings"
        if all_mode:
            c_err_word = "error" if compiled_errors == 1 else "errors"
            c_warn_word = "warning" if compiled_warnings == 1 else "warnings"
            l_err_word = "error" if legacy_errors == 1 else "errors"
            l_warn_word = "warning" if legacy_warnings == 1 else "warnings"
            print(f"\ncompiled: {len(compiled_results)} files · {compiled_errors} {c_err_word} · {compiled_warnings} {c_warn_word}")
            print(f"legacy:   {len(legacy_results)} files · {legacy_errors} {l_err_word} · {legacy_warnings} {l_warn_word}")
            print(f"total:    {n} files · {total_errors} {err_word} · {total_warnings} {warn_word}")
        else:
            file_word = "file" if n == 1 else "files"
            print(f"\n{n} {file_word} · {total_errors} {err_word} · {total_warnings} {warn_word}")

    if permissive:
        sys.exit(0)
    if total_errors > 0:
        sys.exit(1)
    if total_warnings > 0:
        sys.exit(2)
    sys.exit(0)


def _run_check_strict(paths: list[str], catalog: dict) -> tuple[list[dict], int, int]:
    """Run all validators on paths, return (file_results, total_errors, total_warnings)."""
    from validators import schema as schema_v
    from validators import producer_check as producer_v
    from validators import deadcode as deadcode_v

    file_results = []
    for p in paths:
        issues: list[dict] = []
        issues += schema_v.validate(p)
        issues += producer_v.validate(p, catalog)
        issues += deadcode_v.validate(p)
        file_results.append({"path": p, "issues": issues})

    corpus_issues = deadcode_v.validate_corpus(paths)
    corpus_by_file: dict[str, list] = {}
    for issue in corpus_issues:
        corpus_by_file.setdefault(issue["location"]["file"], []).append(issue)
    for fr in file_results:
        fr["issues"] += corpus_by_file.get(fr["path"], [])

    total_errors = sum(1 for fr in file_results for i in fr["issues"] if i["severity"] == "error")
    total_warnings = sum(1 for fr in file_results for i in fr["issues"] if i["severity"] == "warning")
    return file_results, total_errors, total_warnings


def _source_hash(path: str) -> str:
    with open(path, "rb") as f:
        return hashlib.sha256(f.read()).hexdigest()[:16]


def _is_already_compiled(out_path: str, src_path: str) -> bool:
    """Return True if the existing compiled file is up-to-date (same version + same source hash)."""
    if not os.path.exists(out_path):
        return False
    src_hash = _source_hash(src_path)
    try:
        with open(out_path) as f:
            for line in f:
                if not line.startswith("#"):
                    break
                if f"compiler_version: {COMPILER_VERSION}" in line:
                    # also check source hash
                    pass
                if f"source_hash: {src_hash}" in line:
                    return True
    except OSError:
        pass
    return False


def _build_compiled_content(src_path: str, bundle_name: str, checks_passed: list[str]) -> str:
    """Return the compiled YAML content with manifest headers."""
    # Derive the generated_from path relative to mrap-hex integrations root
    # e.g. /Users/mrap/mrap-hex/integrations/kalshi/events/foo.yaml → integrations/kalshi/events/foo.yaml
    abs_path = os.path.abspath(src_path)
    mrap_hex = os.path.expanduser("~/mrap-hex")
    if abs_path.startswith(mrap_hex + os.sep):
        rel = abs_path[len(mrap_hex) + 1:]
    else:
        rel = abs_path

    now = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    src_hash = _source_hash(src_path)

    with open(src_path) as f:
        original = f.read()

    # Strip any existing header comments that start with generated_from/generated_at/etc.
    lines = original.splitlines(keepends=True)
    body_lines = []
    skip_header = True
    for line in lines:
        if skip_header and line.startswith("#"):
            continue
        skip_header = False
        body_lines.append(line)
    body = "".join(body_lines).lstrip("\n")

    header = (
        f"# generated_from: {rel}\n"
        f"# generated_at: {now}\n"
        f"# compiler_version: {COMPILER_VERSION}\n"
        f"# source_hash: {src_hash}\n"
        f"# checks_passed: {', '.join(checks_passed)}\n"
    )
    return header + body


def cmd_compile(args):
    dry_run = getattr(args, "dry_run", False)
    bundle_path = os.path.abspath(args.path)

    # Resolve input files (same logic as check)
    paths = _resolve_check_paths(bundle_path)
    if not paths:
        print(f"No YAML files found at: {args.path}", file=sys.stderr)
        sys.exit(1)

    # Derive bundle name from directory
    if os.path.isfile(bundle_path):
        bundle_name = os.path.basename(os.path.dirname(os.path.dirname(bundle_path)))
    else:
        bundle_name = os.path.basename(bundle_path.rstrip("/"))

    catalog = _build_event_catalog()
    file_results, total_errors, total_warnings = _run_check_strict(paths, catalog)

    if total_errors > 0:
        for fr in file_results:
            for issue in fr["issues"]:
                if issue["severity"] == "error":
                    loc = issue["location"]
                    print(
                        f"{loc.get('file', fr['path'])}:{loc.get('line', '?')}: "
                        f"[{issue['code']}] {issue['message']}",
                        file=sys.stderr,
                    )
        print(
            f"\nCompile failed: {total_errors} error(s). No output written.",
            file=sys.stderr,
        )
        sys.exit(1)

    checks_passed = ["schema", "producer-check", "dead-code"]
    compiled_count = 0
    skipped_count = 0

    for src_path in paths:
        stem = os.path.splitext(os.path.basename(src_path))[0]
        out_name = f"{bundle_name}-{stem}.yaml"
        out_path = os.path.join(POLICIES_DIR, out_name)

        if _is_already_compiled(out_path, src_path):
            print(f"  no changes: {out_name}")
            skipped_count += 1
            continue

        content = _build_compiled_content(src_path, bundle_name, checks_passed)

        if dry_run:
            print(f"  [dry-run] would write: {out_path}")
            print("  --- preview ---")
            for line in content.splitlines()[:8]:
                print(f"  {line}")
            print("  ...")
        else:
            os.makedirs(POLICIES_DIR, exist_ok=True)
            tmp_path = out_path + ".tmp"
            with open(tmp_path, "w") as f:
                f.write(content)
            os.replace(tmp_path, out_path)
            print(f"  compiled: {out_path}")
            compiled_count += 1

    if not dry_run:
        action = "compiled" if compiled_count else "up to date"
        print(
            f"\n{len(paths)} file(s) · {compiled_count} {action} · {skipped_count} skipped"
        )

    sys.exit(0)


def main():
    parser = argparse.ArgumentParser(description="hex-events CLI")
    sub = parser.add_subparsers(dest="command")

    sub.add_parser("status", help="Show daemon and system status")

    hist = sub.add_parser("history", help="Show event timeline")
    hist.add_argument("--since", type=int, default=None, help="Hours to look back")

    insp = sub.add_parser("inspect", help="Inspect a specific event")
    insp.add_argument("event_id", type=int, help="Event ID")

    sub.add_parser("recipes", help="List loaded recipes")

    test = sub.add_parser("test", help="Dry-run a recipe file")
    test.add_argument("recipe_file", help="Path to recipe YAML")

    val_p = sub.add_parser("validate", help="Validate policy schema for all or a specific file")
    val_p.add_argument("file", nargs="?", help="Specific policy file to validate (default: all)")

    graph_p = sub.add_parser("graph", help="Show event dependency graph")

    trace_p = sub.add_parser("trace", help="Show policy evaluation trace for an event")
    trace_p.add_argument("event_id", type=int, nargs="?", help="Event ID to trace")
    trace_p.add_argument("--policy", help="Filter to a specific policy name")
    trace_p.add_argument("--since", type=int, default=None,
                         help="Hours to look back (used with --policy to show history)")
    graph_p.add_argument("--observed", action="store_true",
                         help="Show observed graph from DB (last 7 days)")

    telem_p = sub.add_parser("telemetry", help="Show unified telemetry health overview")
    telem_p.add_argument("--json", action="store_true", help="Output as JSON")

    sub.add_parser("workflows", help="List all workflows with status")

    wf_p = sub.add_parser("workflow", help="Show/manage a workflow")
    wf_p.add_argument("name", help="Workflow name (directory name)")
    wf_p.add_argument("action", nargs="?", choices=["enable", "disable", "status"],
                       help="Action to perform on the workflow")

    le_p = sub.add_parser("list-events", help="Show full event catalog (producers + consumers)")
    le_p.add_argument("--format", choices=["json"], help="Output format")

    check_p = sub.add_parser("check", help="Run all validators against a bundle or policy file")
    check_p.add_argument("path", nargs="?", help="Bundle dir, policy file, or bundle root (omit with --all)")
    check_p.add_argument("--format", choices=["json"], help="Output format")
    check_p.add_argument("--permissive", action="store_true", help="Warn-only mode: always exit 0")
    check_p.add_argument("--all", action="store_true", help=f"Check all policies in {POLICIES_DIR}")

    compile_p = sub.add_parser("compile", help="Compile a bundle: check + write manifest-headed policy files")
    compile_p.add_argument("path", help="Bundle dir or single policy file")
    compile_p.add_argument("--dry-run", action="store_true", dest="dry_run",
                           help="Run checks and show what would be written without writing")

    args = parser.parse_args()
    if args.command == "status":
        cmd_status(args)
    elif args.command == "history":
        cmd_history(args)
    elif args.command == "inspect":
        cmd_inspect(args)
    elif args.command == "recipes":
        cmd_recipes(args)
    elif args.command == "test":
        cmd_test(args)
    elif args.command == "validate":
        cmd_validate(args)
    elif args.command == "graph":
        cmd_graph(args)
    elif args.command == "trace":
        cmd_trace(args)
    elif args.command == "telemetry":
        cmd_telemetry(args)
    elif args.command == "workflows":
        cmd_workflows(args)
    elif args.command == "workflow":
        cmd_workflow(args)
    elif args.command == "list-events":
        cmd_list_events(args)
    elif args.command == "check":
        cmd_check(args)
    elif args.command == "compile":
        cmd_compile(args)
    else:
        parser.print_help()

if __name__ == "__main__":
    main()
