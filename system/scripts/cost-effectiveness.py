#!/usr/bin/env python3
"""Cost-effectiveness engine: which agents produce the most KR movement per dollar?

Correlates agent spending (ledger.jsonl) with KR movement (kr-snapshots.jsonl)
using initiative ownership (initiatives/*.yaml) to attribute KR deltas to agents.

Usage:
    python3 .hex/scripts/cost-effectiveness.py              # full report
    python3 .hex/scripts/cost-effectiveness.py --agent cos   # single agent
    python3 .hex/scripts/cost-effectiveness.py --json        # JSON output
"""

import argparse
import json
import os
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path


sys.path.insert(0, str(Path(__file__).parent))
from lib.hex_utils import get_hex_root

HEX_DIR = str(get_hex_root())
LEDGER_PATH = os.path.join(HEX_DIR, ".hex/cost/ledger.jsonl")
KR_SNAPSHOTS_PATH = os.path.expanduser("~/.hex/audit/kr-snapshots.jsonl")
INITIATIVES_DIR = os.path.join(HEX_DIR, "initiatives")
OUTPUT_PATH = os.path.expanduser("~/.hex/audit/tuning-recommendations.jsonl")


def parse_ts(ts_str):
    """Parse ISO timestamp to datetime. Handles +00:00 and Z suffixes."""
    ts_str = ts_str.strip()
    if ts_str.endswith("Z"):
        ts_str = ts_str[:-1] + "+00:00"
    # Python 3.7+ handles +00:00 in fromisoformat
    try:
        return datetime.fromisoformat(ts_str)
    except ValueError:
        # Fallback: strip fractional seconds
        for fmt in ("%Y-%m-%dT%H:%M:%S%z", "%Y-%m-%dT%H:%M:%S.%f%z"):
            try:
                return datetime.strptime(ts_str, fmt)
            except ValueError:
                continue
        raise


def load_jsonl(path):
    """Load a JSONL file, skipping blank/malformed lines."""
    records = []
    if not os.path.exists(path):
        return records
    with open(path, "r") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                records.append(json.loads(line))
            except json.JSONDecodeError:
                continue
    return records


def load_initiative_ownership(initiatives_dir):
    """Build mapping: initiative_id -> owner agent, and agent -> list of init IDs."""
    # Only import yaml parser if available, otherwise parse manually
    init_to_owner = {}
    owner_to_inits = {}

    if not os.path.isdir(initiatives_dir):
        return init_to_owner, owner_to_inits

    for fname in os.listdir(initiatives_dir):
        if not fname.endswith(".yaml") and not fname.endswith(".yml"):
            continue
        fpath = os.path.join(initiatives_dir, fname)
        init_id = None
        owner = None
        with open(fpath, "r") as f:
            for raw_line in f:
                raw_line = raw_line.strip()
                if raw_line.startswith("id:"):
                    init_id = raw_line.split(":", 1)[1].strip()
                elif raw_line.startswith("owner:"):
                    owner = raw_line.split(":", 1)[1].strip()
                if init_id and owner:
                    break
        if init_id and owner:
            init_to_owner[init_id] = owner
            owner_to_inits.setdefault(owner, []).append(init_id)

    return init_to_owner, owner_to_inits


def aggregate_costs(ledger_records, cutoff, agent_filter=None):
    """Aggregate cost_usd by agent for records after cutoff."""
    agent_costs = {}
    agent_invocations = {}
    for rec in ledger_records:
        agent = rec.get("agent")
        if not agent:
            continue
        if agent_filter and agent != agent_filter:
            continue
        try:
            ts = parse_ts(rec["ts"])
        except (KeyError, ValueError):
            continue
        if ts < cutoff:
            continue
        cost = float(rec.get("cost_usd", 0))
        agent_costs[agent] = agent_costs.get(agent, 0.0) + cost
        agent_invocations[agent] = agent_invocations.get(agent, 0) + 1
    return agent_costs, agent_invocations


def compute_kr_deltas(snapshots, cutoff, init_to_owner):
    """Compute per-agent KR delta count over the window.

    A KR "moved" if its value changed (absolute delta >= 1.0) between the
    earliest and latest snapshots in the window.

    Returns: {agent: number_of_krs_that_moved}
    """
    # Collect all KR keys and their values over time
    kr_values = {}  # kr_key -> [(ts, value)]

    for snap in snapshots:
        try:
            ts = parse_ts(snap["ts"])
        except (KeyError, ValueError):
            continue
        if ts < cutoff:
            continue
        snapshot_data = snap.get("snapshot", {})
        if snapshot_data:
            # Format A: {"snapshot": {"init/kr": value}}
            for kr_key, value in snapshot_data.items():
                try:
                    val = float(value)
                except (TypeError, ValueError):
                    continue
                kr_values.setdefault(kr_key, []).append((ts, val))
        elif "initiative" in snap and "krs" in snap:
            # Format B: {"initiative": "init-xxx", "krs": [{"id": "kr-1", "current": 0.5}]}
            init_id = snap["initiative"]
            for kr in snap.get("krs", []):
                kr_key = f"{init_id}/{kr.get('id', '?')}"
                try:
                    val = float(kr.get("current", 0))
                except (TypeError, ValueError):
                    continue
                kr_values.setdefault(kr_key, []).append((ts, val))

    # For each KR, compute delta between earliest and latest
    kr_deltas = {}  # kr_key -> abs(delta)
    for kr_key, entries in kr_values.items():
        if len(entries) < 2:
            continue
        entries.sort(key=lambda x: x[0])
        earliest_val = entries[0][1]
        latest_val = entries[-1][1]
        delta = abs(latest_val - earliest_val)
        if delta >= 1.0:
            kr_deltas[kr_key] = delta

    # Attribute KR movements to agents via initiative ownership
    # kr_key format: "init-{initiative-id}/kr-{n}" -> strip to get init id
    agent_kr_moves = {}
    for kr_key in kr_deltas:
        # Extract initiative id: everything before the last "/kr-..."
        parts = kr_key.rsplit("/", 1)
        if len(parts) != 2:
            continue
        init_id = parts[0]
        owner = init_to_owner.get(init_id)
        if owner:
            agent_kr_moves[owner] = agent_kr_moves.get(owner, 0) + 1

    return agent_kr_moves, kr_deltas


def generate_recommendation(agent, cost_24h, krs_moved, cost_per_kr, rank, total_agents):
    """Generate a recommendation string and confidence score."""
    bottom_50_threshold = total_agents / 2
    top_25_threshold = total_agents * 0.25

    if krs_moved == 0 and cost_24h > 5.0:
        return "investigate -- spending without progress", 0.8
    if cost_24h > 20.0 and rank > bottom_50_threshold:
        return "reduce wake frequency -- high cost, moderate output", 0.7
    if cost_24h < 2.0 and rank <= top_25_threshold and krs_moved > 0:
        return "increase wake frequency -- low cost, high output", 0.7
    if krs_moved == 0 and cost_24h <= 5.0:
        return "monitor -- low spend, no KR movement yet", 0.5
    if cost_24h > 20.0 and rank <= top_25_threshold:
        return "healthy -- high cost justified by strong KR movement", 0.8
    if cost_24h <= 20.0 and krs_moved > 0:
        return "healthy -- reasonable cost with KR progress", 0.6
    return "review -- moderate cost-effectiveness", 0.5


def run(args):
    now = datetime.now(timezone.utc)
    window_days = args.days
    cutoff = now - timedelta(days=window_days)

    # Load data
    ledger = load_jsonl(LEDGER_PATH)
    snapshots = load_jsonl(KR_SNAPSHOTS_PATH)
    init_to_owner, owner_to_inits = load_initiative_ownership(INITIATIVES_DIR)

    if not ledger:
        print("No cost data found in ledger.", file=sys.stderr)
        sys.exit(1)

    # Aggregate costs
    agent_costs, agent_invocations = aggregate_costs(ledger, cutoff, args.agent)

    # Compute KR deltas
    agent_kr_moves, kr_deltas = compute_kr_deltas(snapshots, cutoff, init_to_owner)

    # Build results for all agents with cost data
    all_agents = set(agent_costs.keys())
    # Also include agents that have KR moves but might not have cost (unlikely but safe)
    if not args.agent:
        all_agents |= set(agent_kr_moves.keys())

    results = []
    for agent in sorted(all_agents):
        total_cost = agent_costs.get(agent, 0.0)
        cost_24h = total_cost / max(window_days, 1)
        krs_moved = agent_kr_moves.get(agent, 0)
        cost_per_kr = total_cost / max(krs_moved, 0.1)
        invocations = agent_invocations.get(agent, 0)

        results.append({
            "agent": agent,
            "cost_total": round(total_cost, 2),
            "cost_24h": round(cost_24h, 2),
            "krs_moved": krs_moved,
            "cost_per_kr_delta": round(cost_per_kr, 2),
            "invocations": invocations,
        })

    # Rank by effectiveness (lower cost_per_kr_delta = more effective)
    # Agents with KR movement are always ranked above those without
    results.sort(key=lambda r: (0 if r["krs_moved"] > 0 else 1, r["cost_per_kr_delta"]))

    total_agents = len(results)
    recommendations = []
    for rank_idx, r in enumerate(results):
        rank = rank_idx + 1
        rec_text, confidence = generate_recommendation(
            r["agent"], r["cost_24h"], r["krs_moved"],
            r["cost_per_kr_delta"], rank, total_agents
        )
        entry = {
            "ts": now.isoformat(),
            "agent": r["agent"],
            "cost_24h": r["cost_24h"],
            "cost_total_window": r["cost_total"],
            "krs_moved": r["krs_moved"],
            "cost_per_kr_delta": r["cost_per_kr_delta"],
            "effectiveness_rank": rank,
            "invocations": r["invocations"],
            "recommendation": rec_text,
            "confidence": confidence,
            "window_days": window_days,
        }
        recommendations.append(entry)

    # Write output
    os.makedirs(os.path.dirname(OUTPUT_PATH), exist_ok=True)
    with open(OUTPUT_PATH, "a") as f:
        for entry in recommendations:
            f.write(json.dumps(entry) + "\n")

    # Display
    if args.json:
        json.dump(recommendations, sys.stdout, indent=2)
        print()
    else:
        print(f"Cost-Effectiveness Report ({window_days}d window)")
        print(f"{'='*72}")
        print(f"{'Agent':<20} {'Cost/day':>9} {'KRs':>5} {'$/KR':>10} {'Rank':>5}  Recommendation")
        print(f"{'-'*20} {'-'*9} {'-'*5} {'-'*10} {'-'*5}  {'-'*30}")
        for entry in recommendations:
            kr_str = str(entry["krs_moved"])
            cost_kr_str = (
                f"${entry['cost_per_kr_delta']:.2f}"
                if entry["krs_moved"] > 0
                else "n/a"
            )
            print(
                f"{entry['agent']:<20} ${entry['cost_24h']:>7.2f} {kr_str:>5} "
                f"{cost_kr_str:>10} {entry['effectiveness_rank']:>5}  "
                f"{entry['recommendation']}"
            )
        print(f"\nTotal agents: {len(recommendations)}")
        print(f"Window: {cutoff.strftime('%Y-%m-%d')} to {now.strftime('%Y-%m-%d')}")
        print(f"Output written to: {OUTPUT_PATH}")


def main():
    parser = argparse.ArgumentParser(
        description="Cost-effectiveness engine: agent spending vs KR movement"
    )
    parser.add_argument("--agent", type=str, default=None, help="Filter to a single agent")
    parser.add_argument("--json", action="store_true", help="Output as JSON")
    parser.add_argument("--days", type=int, default=7, help="Lookback window in days (default: 7)")
    args = parser.parse_args()
    run(args)


if __name__ == "__main__":
    main()
