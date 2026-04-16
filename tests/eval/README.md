# hex eval harness

Behavioral tests that verify a hex install follows the operating model correctly.
Uses `claude -p` (Claude Code print mode) to send prompts and check both response
content and file-system side effects.

## What it tests

| Case | Standing Order | What it verifies |
|------|---------------|-----------------|
| `onboarding` | Session Lifecycle | Fresh install triggers name/onboarding prompt |
| `memory_search` | SO #1 — Search before guessing | Agent finds seeded decision files when asked |
| `persistence` | SO #2 — Persist immediately | Agent writes a decision file after being told one |
| `delegation` | SO #7 — BOI is the default | Agent suggests BOI dispatch for multi-file work |
| `hex_events_routing` | S4 — Use hex-events | Agent suggests hex-events for reactive notifications |
| `startup_loads_context` | Session Lifecycle | /hex-startup reads todo.md and surfaces priorities |

Each case gets its own isolated hex install in a temp directory.

## How to run

### Dry run (no API key needed)

Validates all YAML cases, verifies install.sh works, checks that `claude` CLI is available:

```bash
cd tests/eval
python3 run_eval.py --dry-run
```

Expected output: `6 cases validated, ready for live run.`

### Live run (requires claude CLI + ANTHROPIC_API_KEY)

```bash
# Full suite
python3 run_eval.py --live

# Single case
python3 run_eval.py --live --case onboarding

# Different model
python3 run_eval.py --live --model haiku

# Longer timeout (default 120s)
python3 run_eval.py --live --timeout 180
```

Model shorthands: `sonnet`, `haiku`, `opus` (maps to current claude-*-4-5 IDs).

### Cost estimate

Running the full suite with Sonnet: approximately **$0.50** (6 cases × ~2K input tokens + ~500 output tokens).
Use `--model haiku` for ~$0.05 if you just want a quick check.

## How to add a test case

1. Create `cases/<name>.yaml` with this structure:

```yaml
name: my_case
description: One line describing what behavior this tests
prompt: "The prompt sent to claude -p"
setup: fresh_install  # or: populated
seed_data:            # Only needed for 'populated' setup
  path/relative/to/hex/root.md: |
    File content here
response_checks:
  - name: check_name
    pattern: "(?i)(regex to match in response)"
    description: What the agent should have said
file_checks:
  - name: file_check_name
    path_pattern: "glob/pattern/**/*"
    check: exists    # or: contains
    content: "regex"  # required if check: contains
    description: What file should exist or contain
```

2. Run `--dry-run` to validate it:

```bash
python3 run_eval.py --dry-run --case my_case
```

3. Run live to see if it passes:

```bash
python3 run_eval.py --live --case my_case
```

## Architecture

```
tests/eval/
├── run_eval.py       — Main runner (dry-run + live modes)
├── cases/            — YAML test case definitions
│   ├── onboarding.yaml
│   ├── memory_search.yaml
│   ├── persistence.yaml
│   ├── delegation.yaml
│   ├── hex_events_routing.yaml
│   └── startup_loads_context.yaml
└── README.md         — This file
```

The runner:
1. Loads cases from `cases/*.yaml`
2. For each case: creates a fresh hex install in a temp dir, applies seed data if needed
3. Calls `claude -p "<prompt>" --cwd <hex_dir> --model <model>`
4. Checks response text against regex patterns
5. Checks filesystem for expected files/content
6. Prints a pass/fail summary table
