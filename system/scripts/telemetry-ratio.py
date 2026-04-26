#!/usr/bin/env python3
"""
telemetry-ratio.py — Input:Output ratio calculator for feedback loop health.

Usage:
    python3 telemetry-ratio.py              # last 24h, all surfaces
    python3 telemetry-ratio.py --hours 6    # last 6h
    python3 telemetry-ratio.py --surface pulse  # pulse only
    python3 telemetry-ratio.py --json       # machine-readable JSON output

Alert threshold: < 50% ratio = ALERT (override with HEX_TELEMETRY_RATIO_THRESHOLD env var)
"""
import argparse
import json
import os
import sqlite3
import sys
from datetime import datetime, timezone, timedelta

DB_PATH = os.path.join(os.path.dirname(__file__), "..", "telemetry", "events.db")
DB_PATH = os.path.normpath(DB_PATH)

DEFAULT_THRESHOLD = int(os.environ.get("HEX_TELEMETRY_RATIO_THRESHOLD", "50"))

SURFACES = {
    "pulse": {
        "label": "pulse",
        "inputs":  ["pulse.message.received"],
        "outputs": ["pulse.message.acted_on", "pulse.message.routed", "pulse.message.responded"],
    },
    "slack": {
        "label": "slack",
        "inputs":  ["slack.message.received"],
        "outputs": ["slack.message.responded"],
    },
    "captures": {
        "label": "captures",
        "inputs":  ["capture.created"],
        "outputs": ["capture.triaged", "capture.dispatched"],
    },
    "boi": {
        "label": "boi",
        "inputs":  ["boi.spec.dispatched"],
        "outputs": ["boi.spec.completed", "boi.spec.failed"],
        # COALESCE order: cli emits spec_id, boi-worker emits queue_id; both hold the queue ID
        "deduplicate_by": ["spec_id", "queue_id"],
    },
    "agent-inbox": {
        "label": "agent-inbox",
        "inputs":  ["agent.message.sent"],
        "outputs": ["agent.message.processed"],
    },
    "policies": {
        "label": "policies",
        "inputs":  ["policy.fired"],
        "outputs": ["policy.action.completed"],
    },
}


def query_count(conn: sqlite3.Connection, event_types: list[str], since: str) -> int:
    if not event_types:
        return 0
    placeholders = ",".join("?" * len(event_types))
    row = conn.execute(
        f"SELECT COUNT(*) FROM events WHERE event_type IN ({placeholders}) AND ts >= ?",
        [*event_types, since],
    ).fetchone()
    return row[0] if row else 0


def query_count_unique(
    conn: sqlite3.Connection,
    event_types: list[str],
    payload_keys: list[str],
    since: str,
) -> int:
    """Count distinct payload IDs across event_types since the given timestamp.

    payload_keys is tried in order via COALESCE so that events from different
    sources (e.g. cli uses spec_id, boi-worker uses queue_id) all resolve to
    the same canonical ID.
    """
    if not event_types:
        return 0
    type_placeholders = ",".join("?" * len(event_types))
    coalesce_expr = ", ".join(
        f"json_extract(payload, '$.{k}')" for k in payload_keys
    )
    row = conn.execute(
        f"SELECT COUNT(DISTINCT COALESCE({coalesce_expr})) "
        f"FROM events WHERE event_type IN ({type_placeholders}) AND ts >= ?",
        [*event_types, since],
    ).fetchone()
    return row[0] if row else 0


def compute_ratio(inputs: int, outputs: int) -> float:
    if inputs == 0:
        return 1.0  # no inputs → trivially healthy (100%)
    return outputs / inputs


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Calculate input:output ratio for each feedback surface.",
        epilog="Ratio < HEX_TELEMETRY_RATIO_THRESHOLD (default 50%) is flagged as ALERT.",
    )
    parser.add_argument("--hours", type=float, default=24.0,
                        help="Look-back window in hours (default: 24)")
    parser.add_argument("--surface", choices=list(SURFACES.keys()),
                        help="Restrict output to a single surface")
    parser.add_argument("--json", dest="json_output", action="store_true",
                        help="Emit machine-readable JSON instead of table")
    parser.add_argument("--threshold", type=int, default=DEFAULT_THRESHOLD,
                        help=f"Alert threshold percent (default: {DEFAULT_THRESHOLD})")
    args = parser.parse_args()

    since_dt = datetime.now(timezone.utc) - timedelta(hours=args.hours)
    since_str = since_dt.strftime("%Y-%m-%dT%H:%M:%S")

    if not os.path.exists(DB_PATH):
        print(f"ERROR: telemetry DB not found at {DB_PATH}", file=sys.stderr)
        sys.exit(1)

    conn = sqlite3.connect(DB_PATH)

    surfaces_to_check = (
        {args.surface: SURFACES[args.surface]} if args.surface else SURFACES
    )

    results = []
    for key, cfg in surfaces_to_check.items():
        dedup_keys = cfg.get("deduplicate_by")
        if dedup_keys:
            inp = query_count_unique(conn, cfg["inputs"], dedup_keys, since_str)
            out = query_count_unique(conn, cfg["outputs"], dedup_keys, since_str)
            unique_inputs: int | None = inp
            unique_outputs: int | None = out
        else:
            inp = query_count(conn, cfg["inputs"], since_str)
            out = query_count(conn, cfg["outputs"], since_str)
            unique_inputs = None
            unique_outputs = None
        ratio = compute_ratio(inp, out)
        pct = int(ratio * 100)
        status = "ALERT" if inp > 0 and pct < args.threshold else "OK"
        results.append({
            "surface": cfg["label"],
            "inputs": inp,
            "outputs": out,
            "unique_inputs": unique_inputs,
            "unique_outputs": unique_outputs,
            "ratio_pct": pct,
            "status": status,
        })

    total_inputs = sum(r["inputs"] for r in results)
    total_outputs = sum(r["outputs"] for r in results)
    overall_ratio = compute_ratio(total_inputs, total_outputs)
    overall_pct = int(overall_ratio * 100)
    overall_status = "ALERT" if total_inputs > 0 and overall_pct < args.threshold else "OK"

    conn.close()

    if args.json_output:
        print(json.dumps({
            "window_hours": args.hours,
            "since": since_str,
            "threshold_pct": args.threshold,
            "surfaces": results,
            "overall": {
                "inputs": total_inputs,
                "outputs": total_outputs,
                "ratio_pct": overall_pct,
                "status": overall_status,
            },
        }, indent=2))
        return

    hours_label = f"{args.hours:.0f}h" if args.hours == int(args.hours) else f"{args.hours}h"
    print(f"\nFEEDBACK LOOP HEALTH — Last {hours_label}  (threshold {args.threshold}%)")
    print(f"{'Surface':<16} {'Input':>22} {'Output':>22} {'Ratio':>7}    Status")
    print("-" * 78)
    for r in results:
        ratio_str = f"{r['ratio_pct']}%"
        if r["unique_inputs"] is not None:
            inp_str = f"{r['inputs']} ({r['unique_inputs']} unique)"
            out_str = f"{r['outputs']} ({r['unique_outputs']} unique)"
        else:
            inp_str = str(r["inputs"])
            out_str = str(r["outputs"])
        print(f"{r['surface']:<16} {inp_str:>22} {out_str:>22} {ratio_str:>7}    {r['status']}")

    print()
    print(f"Overall: {total_inputs} inputs, {total_outputs} outputs, {overall_pct}% acted-on rate  [{overall_status}]")
    print()

    if any(r["status"] == "ALERT" for r in results) or overall_status == "ALERT":
        sys.exit(2)  # non-zero exit so callers can detect degraded state


if __name__ == "__main__":
    main()
