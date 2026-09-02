---
name: vibe-to-prod
description: >
  Run the vibe-to-production pipeline on any Python project. Assesses code quality,
  generates characterization tests, prioritizes refactoring targets, and dispatches
  BOI specs for each phase. Use when the user says "vibe to prod", "harden this",
  "production-ready", "assess this project", "run assessment", or references the
  vibe-to-production playbook.
version: 1.0.0
---

# Vibe-to-Production Pipeline

Transitions a vibe-coded Python project to production quality through 6 phases,
each dispatched as BOI specs.

## Usage

```
/vibe-to-prod <project-path> [--phase N] [--output-dir PATH]
```

- `<project-path>` — Absolute path to the Python source directory (required)
- `--phase N` — Run only phase N (1-6). Default: start from phase 1 or resume from last completed phase.
- `--output-dir PATH` — Where to write assessment outputs. Default: `projects/<project-name>-v2p/`

## Phase Overview

| Phase | Name | BOI Spec | Input | Output |
|-------|------|----------|-------|--------|
| 1 | Assessment | `assess-<project>.spec.toml` | Source code | Metrics JSON, priority list |
| 2 | Safety Net | `characterize-<project>.spec.toml` | P1-P2 modules from Phase 1 | Characterization tests |
| 3 | Prioritization | Inline (no BOI) | Phase 1 metrics | Prioritized backlog |
| 4 | Execution | `refactor-<module>.spec.toml` (one per P1/P2 module) | Char tests + priority list | Refactored code |
| 5 | Verification | `verify-<project>.spec.toml` | Before/after metrics | Improvement report |
| 6 | Maintain | Inline (no BOI) | Verification report | CI config, CLAUDE.md updates |

## Orchestration Flow

### When invoked:

1. **Validate inputs:**
   - Confirm `<project-path>` exists and contains `.py` files
   - Set `OUTPUT_DIR` (default: `$HEX_DIR/projects/<project-name>-v2p/`)
   - Set `PROJECT_NAME` from directory basename

2. **Check pipeline state:**
   - Read `$OUTPUT_DIR/pipeline-state.json` if it exists
   - Resume from the last completed phase
   - If no state file, start from Phase 1

3. **Execute the requested phase:**

#### Phase 1: Assessment
- Install tools if missing: `pip install vulture radon pydeps bandit ruff`
- Generate assessment BOI spec from template:
  ```bash
  bash .hex/skills/vibe-to-prod/scripts/expand_template.sh \
    .hex/skills/vibe-to-prod/templates/assess.spec.toml \
    $OUTPUT_DIR/assess-$PROJECT_NAME.spec.toml \
    PROJECT_PATH=$PROJECT_PATH \
    PROJECT_NAME=$PROJECT_NAME \
    OUTPUT_DIR=$OUTPUT_DIR
  ```
- Dispatch: `boi dispatch $OUTPUT_DIR/assess-$PROJECT_NAME.spec.toml`
- On completion: run priority computation script, save `$OUTPUT_DIR/priorities.json`
- Update pipeline state

#### Phase 2: Safety Net
- Read `$OUTPUT_DIR/priorities.json` for P1-P2 modules
- Set `MODULE_LIST` to comma-separated list of P1-P2 module paths (e.g. `src/foo.py,src/bar.py`)
- Generate characterization test spec from template:
  ```bash
  bash .hex/skills/vibe-to-prod/scripts/expand_template.sh \
    .hex/skills/vibe-to-prod/templates/characterize.spec.toml \
    $OUTPUT_DIR/characterize-$PROJECT_NAME.spec.toml \
    PROJECT_PATH=$PROJECT_PATH \
    PROJECT_NAME=$PROJECT_NAME \
    OUTPUT_DIR=$OUTPUT_DIR \
    MODULE_LIST=$MODULE_LIST
  ```
- Dispatch: `boi dispatch $OUTPUT_DIR/characterize-$PROJECT_NAME.spec.toml`
- On completion: verify all characterization tests pass
- Update pipeline state

#### Phase 3: Prioritization (inline, no BOI)
- Read Phase 1 assessment data
- Run `scripts/compute_priorities.py` against the metrics
- Present prioritized backlog to user for review
- User confirms or adjusts the priority order
- Save confirmed priorities to `$OUTPUT_DIR/confirmed-priorities.json`
- Update pipeline state

#### Phase 4: Execution
- For each P1 module (then P2), generate a refactoring BOI spec from template:
  ```bash
  # Set MODULE_PATH, MODULE_NAME, CC, MI from priorities.json for each module
  bash .hex/skills/vibe-to-prod/scripts/expand_template.sh \
    .hex/skills/vibe-to-prod/templates/refactor.spec.toml \
    $OUTPUT_DIR/refactor-$MODULE_NAME.spec.toml \
    PROJECT_PATH=$PROJECT_PATH \
    MODULE_PATH=$MODULE_PATH \
    MODULE_NAME=$MODULE_NAME \
    CC=$CC \
    MI=$MI
  ```
- One BOI spec per module, one concern per task within the spec
- Dispatch sequentially (each spec must pass char tests before next):
  `boi dispatch $OUTPUT_DIR/refactor-$MODULE_NAME.spec.toml`
- Update pipeline state after each module completes

#### Phase 5: Verification
- Generate verification BOI spec from template
- Template: `.hex/skills/vibe-to-prod/templates/verify.spec.toml`
- **IMPORTANT:** Pass `SKILL_DIR` when expanding this template so helper scripts resolve correctly:
  ```bash
  bash .hex/skills/vibe-to-prod/scripts/expand_template.sh \
    .hex/skills/vibe-to-prod/templates/verify.spec.toml \
    $OUTPUT_DIR/verify-$PROJECT_NAME.spec.toml \
    PROJECT_PATH=$PROJECT_PATH \
    PROJECT_NAME=$PROJECT_NAME \
    OUTPUT_DIR=$OUTPUT_DIR \
    SKILL_DIR=$(realpath .hex/skills/vibe-to-prod)
  ```
- Captures after-metrics, runs comparison, produces improvement report
- Update pipeline state

#### Phase 6: Maintain (inline, no BOI)
- Generate CI quality gate config (GitHub Actions YAML)
- Update project's CLAUDE.md with refactoring context
- Set up boundary enforcement (pydeps cycle check)
- Mark pipeline complete

4. **After each phase:**
   - Update `$OUTPUT_DIR/pipeline-state.json`
   - Update landings if applicable (SO #9)
   - Present phase summary to user

## Pipeline State File

`$OUTPUT_DIR/pipeline-state.json`:
```json
{
  "project_name": "boi",
  "project_path": "$BOI_SRC",
  "output_dir": "$HEX_DIR/projects/boi-v2p",
  "started": "2026-03-16T19:00:00",
  "phases": {
    "1": {"status": "completed", "completed_at": "2026-03-16T19:30:00", "spec_id": "Sgzffh520"},
    "2": {"status": "completed", "completed_at": "2026-03-16T20:00:00", "spec_id": "Tqk3xhw02"},
    "3": {"status": "completed", "completed_at": "2026-03-16T20:05:00"},
    "4": {"status": "in_progress", "modules_completed": ["status.py"], "modules_remaining": ["daemon_ops.py"]},
    "5": {"status": "not_started"},
    "6": {"status": "not_started"}
  }
}
```

## Helper Scripts

All scripts live at `.hex/skills/vibe-to-prod/scripts/`:

| Script | Purpose | Usage |
|--------|---------|-------|
| `compute_priorities.py` | Compute P0-P5 rankings from assessment JSON | `python3 compute_priorities.py <output_dir>` |
| `compare_metrics.py` | Before/after metrics comparison | `python3 compare_metrics.py <output_dir>` |
| `expand_template.sh` | Expand `{{VAR}}` placeholders in templates | `bash expand_template.sh <template> <output> KEY=VALUE ...` |

## BOI Spec Templates

All templates live at `.hex/skills/vibe-to-prod/templates/`:

| Template | Phase | Description |
|----------|-------|-------------|
| `assess.spec.toml` | 1 | Run all 5 analysis tools, save raw JSON, compute before-metrics |
| `characterize.spec.toml` | 2 | Generate inline-snapshot characterization tests for target modules |
| `refactor.spec.toml` | 4 | Per-module constraint-based refactoring with safety checks |
| `verify.spec.toml` | 5 | Re-run assessment, compare before/after, produce improvement report |

## Reference

Full playbook: `projects/system-improvement/vibe-to-production-playbook.md`
Assessment template: `projects/system-improvement/boi-assessment-spec-template.md`
