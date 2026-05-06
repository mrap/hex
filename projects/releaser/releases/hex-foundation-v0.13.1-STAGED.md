# hex-foundation v0.13.1 Release Notes

**Shipped:** 2026-05-06
**SHA:** ffc6f86
**Tag:** v0.13.1
**GitHub Release:** https://github.com/mrap/hex/releases/tag/v0.13.1
**SLA:** Met (~29h from oldest commit fe094cb)

---

## What's in this release

### Fixed (6 commits)

- **check_66 restored in hex-doctor**: `fe094cb` streaming rewrite accidentally deleted the hex events status parse-failure absorption check. Restored verbatim from 704fc53. Doctor now surfaces broken policy paths and parse errors again. (ffc6f86)
- **Doctor streaming**: Doctor command output now streams live via `cmd.spawn()` + `Stdio::inherit()` instead of appearing all at once after completion. (fe094cb)
- **Doctor path fixes**: `run-startup-checks.sh`, `run-memory-checks.sh`, `run-landings-workspace-checks.sh` corrected stale `$HEX_DIR/.claude` path to `$HEX_DIR/.hex` — was causing 5 spurious ERRORs on fresh installs. (fe094cb)
- **hex-doctor streaming rewrite**: tee+PIPESTATUS pattern, combined EXIT traps (prevents second trap overwriting first), explicit mid-pipeline failure surface. (fe094cb)
- **BOI doctor check rewrite**: LaunchAgent-aware detection replaces pgrep-based approach (missed managed processes). (512dc48)
- **Harness compat patch**: types.rs Vec<String> to + new struct fields wired up in message.rs, state.rs, wake.rs. (05411dc)

### Added

- **`WakeConfig.skip_llm`**: Harness field — health-probe agents bypass LLM call while still firing wake-start audit, draining inbox, and saving state. `#[serde(default)]` for backwards compat. (fe094cb, 05411dc)
- **`system/scripts/health/check-message-roundtrip.sh`**: End-to-end validation of skip_llm health-probe wake path. (512dc48)
- **`system/scripts/health/check-career-pipeline.sh`**: Career email pipeline health check — validates draft existence, policy load, optional dry-run send. Sanitize-clean (env-var paths, example addresses). (512dc48)
- **`system/scripts/doctor-checks/boi.sh`**: BOI daemon doctor check with LaunchAgent-aware detection. (512dc48)
- **`system/scripts/hex-watcher`**: Minimal tmux BOI status pane (one-shot or `--watch` loop). (512dc48)

---

## Gates

| Gate | Status |
|------|--------|
| Clean working tree | PASS |
| Version bump (0.13.0 → 0.13.1) | PASS |
| Docker E2E (env resolution) | PASS |
| Docker E2E (regression suite) | PASS |
| Sanitize (no hardcoded paths) | CLEAN |
| Codex parity (7/7) | PASS |
| Autonomy regression | PASS |
| Sentinel SA-028 | PASS |

---

## Security

Sentinel SA-028 PASS — no new LOWs introduced. LOW-V14-1 carry-forward (unquoted `${HEX_DIR}` in YAML command: fields in 5 policy templates) is non-blocking; targeted for v0.13.2 follow-up.

---

## Carry-forward: v0.14.0 target

Harness backlog auto-promotion (seed_backlog_from_md, seed_backlog_from_charter, auto_promote_from_backlog) deferred again — same incomplete state as in v0.13.0. BOI spec needed to implement supporting modules in queue.rs, cost.rs, messaging.rs, and AgentState fields.
