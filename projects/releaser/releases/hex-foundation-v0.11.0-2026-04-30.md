# hex-foundation v0.11.0 — full mrap-hex sync sweep

**Date:** 2026-04-30
**Tag:** v0.11.0
**Base:** v0.10.1 (releaser-unblock fix)

## Highlights

Largest sync to date — 93 atomic units propagated from mrap-hex
(the personal hex instance) to hex-foundation (the canonical repo).
50 ship as-is; 43 underwent sanitization (path/hostname/name scrub)
and were verified by sanitize-check.sh.

## What landed

### New subsystems
- **spec-tool** — full spec browsing + critic-loop UI (server.py + frontend)
- **vibe-to-prod skill** — Python project hardening pipeline
- **conjecture-criticism skill** — adversarial analysis framework
- **hex-fleet** — system health monitor + LaunchAgent
- **boi-pm** — BOI process monitor + LaunchAgent
- **hex-overseer** — self-tuning monitor layer
- **pulse** — Pulse dashboard with mock data + E2E test harness
- **comments-service** — generic in-page commenting widget
- **sse-bus** — generic SSE relay + event bridge

### Improvements
- shared `hex_utils.py` library
- 7 metrics scripts (continuity, done-claim, frustration, loop-waste, etc.)
- 6 doctor-checks subsystem
- 16 health-checks (agent memory, BOI dispatch, cc-connect, MCP servers, etc.)
- skills: memory, hex-event, hex-save, hex-switch, x-twitter, hex-ideate,
  hex-triage, hex-upgrade, hex-sync-base, secret-intake, boi-delegation
- 30+ MCP integration health-check wrappers

### Fixed
- Build: hex_bytes::encode (external dep aliased) + integration tests
  renamed hex_agent → hex (post-v0.8.0 crate rename)

## Excluded from this sync (personal, stays in mrap-hex only)
- Mike's brand-publish suite (Publer / @mikerapadas voice)
- Personal financial / tax dashboards
- claude-compare (Mike vs Whitney behavioral diff tool)
- mirofish deployment scripts (specific GCE VM config)
- Personal Slack channel routing scripts
- sync-guard.sh + sync-secrets.sh (operate ON the personal repo)
- .claude/commands/ — Mike-specific workflow commands

## Source

- Proposal: projects/releaser/v0.11.0/proposal.md (in mrap-hex)
- Classification: 8 parallel Explore agents per subsystem
