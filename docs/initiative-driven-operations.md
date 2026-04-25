# Initiative-Driven Operations

**Version:** 1.0  
**Date:** 2026-04-24  
**Status:** Canonical reference  
**Authority:** Red (requires Mike's approval for standing-order changes in §7)

---

## 1. The Rule

**All agent-dispatched work must trace to an initiative → KR → experiment → spec chain.**

This is not a suggestion. It is the operating invariant that makes measurable outcomes possible. When agents dispatch work without linking it to an initiative, the work happens but nobody knows which goal it served or whether it moved the needle. That is a failure mode.

The three permitted states for any dispatched BOI spec:

| State | Required field |
|-------|----------------|
| Linked to initiative | `initiative: init-<id>` in spec header |
| Linked to experiment | `experiment: exp-NNN` in spec header |
| Emergency bypass | `emergency: true` in spec header + retroactive link within 48h |

Specs that lack all three fields are rejected at dispatch time. No exceptions.

---

## 2. The Primitives

### Initiative

An initiative is a measurable goal with a time horizon, key results, and an owner.

```yaml
# initiatives/<slug>.yaml
id: init-reduce-mike-on-loop
title: Reduce Mike on the Loop
owner: fleet-lead
horizon: 2026-06-01
krs:
  - id: kr-1
    description: "Frustration signal rate < 2 sessions/48h"
    metric: frustration_signal_sessions_48h
    target: 2
    current: 6        # updated by hex-initiative-loop on each agent wake
    status: in_progress
```

**Where:** `initiatives/*.yaml`  
**CLI:** `python3 .hex/scripts/hex-initiative.py <status|measure|review> <id>`

### Key Result (KR)

A KR is a concrete, measurable outcome within an initiative. Each KR maps to a metric with a target value and direction. KRs are the unit of progress. An initiative is complete when all KRs are met.

### Experiment

An experiment tests whether a specific change achieves a KR. It has a hypothesis, a baseline, measurable metrics, and a verdict.

```yaml
# experiments/exp-NNN-<slug>.yaml
id: exp-005
title: Ownership Overhaul — Charter v1.1 User-Outcome Metrics
state: BASELINE          # DRAFT → BASELINE_COLLECTED → ACTIVE → MEASURING → VERDICT
initiative: init-reduce-mike-on-loop
hypothesis: "If we enforce user-outcome metrics via charter v1.1, ..."
metrics:
  primary:
    name: frustration_signal_sessions_48h
    command: "python3 ..."    # must be automatable, no manual checks
    direction: lower_is_better
baseline:
  collected_at: "2026-04-25T01:03:43+00:00"
  values:
    frustration_signal_sessions_48h: 0.0
```

**Where:** `experiments/*.yaml`  
**CLI:** `python3 .hex/scripts/hex-experiment.py <create|baseline|activate|measure|verdict> <id>`

### Spec

A spec is the implementation task. It delivers the change that the experiment tests. A spec must link back to its experiment or initiative via the spec header.

```markdown
# My Feature Spec

**Initiative:** init-reduce-mike-on-loop
**Experiment:** exp-005
...
```

**Where:** dispatched via `boi dispatch --spec <path>`  
**Relationship:** one experiment may link to multiple specs; one spec links to one experiment or initiative.

### Chain

```
Initiative (goal)
  └── Key Result (measurable target)
        └── Experiment (hypothesis + baseline + verdict)
              └── Spec (implementation task)
```

---

## 3. The Enforcement Gate

### Architecture

The gate is implemented as Option D (hybrid): a primary blocking gate at dispatch time + a secondary async audit policy.

**Primary gate** — `~/.boi/src/lib/cli_ops.py::dispatch()`  
Before enqueueing any spec, the BOI CLI reads the spec content and checks for linkage fields. If missing, dispatch is rejected immediately with an actionable error.

**Secondary audit** — `~/.hex-events/policies/initiative-enforcement.yaml`  
After every `boi.spec.dispatched` event, the policy reads the spec and checks linkage. This catches specs that bypass the CLI (direct DB writes, daemon-internal dispatch). If unlinked, it emits `boi.spec.unlinked`, which hex-ops escalates. If emergency, it emits `boi.spec.emergency` for the audit trail.

### What the Primary Gate Checks

```
1. Read spec file content
2. If emergency: true → BYPASS (log audit entry, proceed)
3. If initiative: <id> OR experiment: <id> found → ALLOW
4. Otherwise → REJECT with error message
```

The check is case-insensitive for field names. Values are not validated against the actual initiative/experiment registry at dispatch time — registry validation is the secondary policy's job.

### Supported Field Formats

**Markdown spec header:**
```markdown
**Initiative:** init-reduce-mike-on-loop
**Experiment:** exp-005
**Emergency:** true
```

**YAML spec header:**
```yaml
initiative: init-reduce-mike-on-loop
experiment: exp-005
emergency: true
```

### Rejection Error Message

```
ERROR: Spec must link to an initiative or experiment.

Add one of these to your spec header:
  **Initiative:** init-<id>      (markdown)
  **Experiment:** exp-NNN        (markdown)
  initiative: init-<id>          (YAML)
  experiment: exp-NNN            (YAML)

To bypass for emergency fixes, add:
  **Emergency:** true            (markdown)
  emergency: true                (YAML)

Emergency bypasses are audited. The work must be retroactively
linked to an initiative within 48h or flagged as an orphan.
```

---

## 4. The Agent Loop

Every agent that owns initiatives must invoke the initiative loop on each wake. The loop is executable, not textual.

### Script

```bash
python3 .hex/scripts/hex-initiative-loop.py --agent <agent-id>
```

Options:
- `--agent <id>` — required; runs the loop for initiatives owned by this agent
- `--dry-run` — preview actions without executing (for testing/debugging)
- `--initiative <id>` — scope to a single initiative

### The 7-Step Procedure

For each owned initiative, the loop executes in this order:

1. **Measure KR progress.** Run `hex initiative measure <id>` to update `current` values from live metric commands. If a KR's current value crosses its target: emit `initiative.kr.met` event and update status.

2. **Run verdicts on aged MEASURING experiments.** Find experiments in `MEASURING` state that are ≥48h old. Run `hex experiment verdict <id>` for each. PASS → adopt; FAIL → rollback plan executes.

3. **Propose experiments for uncovered KRs.** For any KR with no active experiment (no experiment in `DRAFT`, `BASELINE_COLLECTED`, `ACTIVE`, or `MEASURING` state linked to this KR), generate a proposed experiment YAML and write it to `experiments/` with `state: DRAFT`.

4. **Collect baselines for stalled DRAFT experiments.** Find experiments in `DRAFT` state that are ≥24h old with no baseline. Run `hex experiment baseline <id>` for each.

5. **Emit activation signal for ready experiments.** Find experiments in `BASELINE_COLLECTED` state. Emit `experiment.ready_for_activation` event for each so the owning agent or Mike can activate.

6. **Check for experiments awaiting measurement.** Find experiments in `ACTIVE` state where `min_cycles_before_measure` cycles have elapsed. Run `hex experiment measure <id>` and transition to `MEASURING`.

7. **Escalate approaching horizons.** If the initiative horizon is within 14 days and any KRs are unmet, emit an escalation event and post to `the configured escalation channel`.

### JSON Output

The loop outputs a JSON summary of all actions taken for the audit trail:

```json
{
  "agent": "cos",
  "run_at": "2026-04-25T01:30:00+00:00",
  "dry_run": false,
  "actions": [
    {"type": "kr_met", "initiative": "init-responsive-ui", "kr": "kr-1"},
    {"type": "baseline_collected", "experiment": "exp-003"},
    {"type": "experiment_proposed", "initiative": "init-memory-evolution", "kr": "kr-2", "file": "experiments/exp-draft-kr2.yaml"}
  ],
  "errors": []
}
```

### When to Invoke

Each agent invokes the loop as the first action of its wake. It replaces the textual description in the CoS charter (item 3-5 of the agent loop). Charter references to "run the initiative loop" mean: execute this script.

---

## 5. The Experiment Lifecycle

```
DRAFT
  │  (baseline collected via hex experiment baseline)
  ▼
BASELINE_COLLECTED
  │  (agent or Mike runs hex experiment activate)
  ▼
ACTIVE
  │  (min_cycles_before_measure cycles elapsed, agent runs hex experiment measure)
  ▼
MEASURING
  │  (≥48h elapsed OR explicit trigger, agent runs hex experiment verdict)
  ▼
VERDICT
  ├── PASS  → change is adopted; KR progress updated
  └── FAIL  → rollback_plan.commands execute; initiative loop proposes next experiment
```

### Transition Triggers

| Transition | Triggered by | Command |
|------------|-------------|---------|
| DRAFT → BASELINE_COLLECTED | Agent (loop step 4) or manually | `hex experiment baseline <id>` |
| BASELINE_COLLECTED → ACTIVE | Agent (loop step 5 signal) or Mike | `hex experiment activate <id>` |
| ACTIVE → MEASURING | Agent (loop step 6) | `hex experiment measure <id>` |
| MEASURING → VERDICT | Agent (loop step 2, ≥48h) | `hex experiment verdict <id>` |

### What Must Be Automatable

Every metric `command` in an experiment YAML must be a shell command that:
- Exits with code 0 on success
- Prints a single number to stdout
- Runs without human interaction
- Completes in under 60 seconds

"Check manually" is not an acceptable metric command. If you cannot automate the measurement, the experiment is not ready to transition to ACTIVE.

---

## 6. The Emergency Escape Hatch

### When to Use

The `emergency: true` field is for:
- Production incidents requiring immediate mitigation
- Security patches that cannot wait for initiative linkage
- Inline fixes discovered mid-session that must proceed now

It is **not** for:
- Work that simply wasn't planned in advance
- Convenience bypasses when you forgot to link an initiative
- Avoiding the overhead of checking active initiatives

### What Happens When You Use It

1. The primary gate allows dispatch and logs an audit entry: `boi.spec.emergency`
2. The secondary policy emits `boi.spec.emergency` (not `boi.spec.unlinked`) — hex-ops notes this but does not escalate immediately
3. The spec executes normally

### Post-Emergency Obligation

Within 48 hours of an emergency spec completing, the owning agent must:
1. Identify which initiative the work served (or determine it serves none)
2. Update the spec's header with the appropriate `initiative:` or `experiment:` field retroactively
3. If no initiative applies, flag the spec as an orphan in `projects/hex-ops/orphan-log.md`

If neither action is taken within 48h, hex-ops flags the spec as unlinked in its next sweep and escalates to Mike.

### Auditing

Emergency bypasses are queryable:
```bash
python3 .hex/scripts/hex-experiment.py list --state emergency 2>/dev/null
```

---

## 7. Proposed CLAUDE.md Changes

The following additions to `CLAUDE.md` encode this operating model as standing orders. These require Mike's approval (Red authority) before taking effect. They are documented here for review.

### Proposed addition to "Operating Model" section

```markdown
### Initiative-Driven Operations

All agent-dispatched work must trace to an initiative → KR → experiment → spec chain.
Before dispatching any BOI spec, identify which initiative it serves. If no initiative
covers the work, either:
  (a) Propose a new initiative (escalate to Mike for approval), or
  (b) Use emergency: true if the work cannot wait, with retroactive linkage within 48h.

Reference: docs/initiative-driven-operations.md
```

### Proposed addition to "Agent Wake Protocol" section (or equivalent)

```markdown
### Initiative Loop (every wake)

On each wake, before handling the user request, invoke:

    python3 .hex/scripts/hex-initiative-loop.py --agent <your-agent-id>

This runs the 7-step initiative loop: measures KR progress, advances stalled experiments,
proposes new experiments for uncovered KRs, collects baselines, and escalates approaching
horizons. Output is a JSON audit summary written to the telemetry stream.

Agents that own no initiatives skip this step.
```

### Proposed Layer 2 mechanism

```markdown
**Initiative Enforcement Layer**

The BOI CLI enforces initiative linkage at dispatch time. The hex-events system audits
all dispatched specs asynchronously. Neither can be bypassed without using emergency: true.

When a spec is rejected at dispatch, the error message provides the list of active
initiatives and the correct field format. The correct response is to link the spec, not
to retry without changes.
```

---

## Reference

| Resource | Location |
|----------|----------|
| Initiative files | `initiatives/*.yaml` |
| Experiment files | `experiments/*.yaml` |
| Initiative CLI | `python3 .hex/scripts/hex-initiative.py` |
| Experiment CLI | `python3 .hex/scripts/hex-experiment.py` |
| Initiative loop script | `python3 .hex/scripts/hex-initiative-loop.py` |
| Enforcement gate implementation | `~/.boi/src/lib/cli_ops.py` |
| Audit policy | `~/.hex-events/policies/initiative-enforcement.yaml` |
| Gate design rationale | `projects/hex-ops/analysis/initiative-enforcement-gate-2026-04-24.md` |
| Experiment schema | `specs/experiment-format.md` |
