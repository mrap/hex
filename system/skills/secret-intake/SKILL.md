---
name: secret-intake
description: >
  Secure credential intake via local web form. Spin up a one-page server on
  Tailscale, paste keys, upload PEM/JSON files, hit submit — secrets land in
  .hex/secrets/ with 600 perms and auto-sync to launchctl + cc-connect.
version: 1.0.0
---

# Secret Intake

Reusable local web form for ingesting API keys, tokens, and key files into hex.

## When to Use

- Adding credentials for a new service or institution
- Rotating or updating existing API keys
- Onboarding a new integration that needs secrets
- Any time secrets need to get from a browser into `.hex/secrets/`

## How to Start

```bash
bash $HEX_DIR/.hex/skills/secret-intake/scripts/start.sh
```

Then give the user the Tailscale URL: `https://<your-tailscale-hostname>/secrets`

The server runs on `:9877` locally. hex-router fronts it at `/secrets` with TLS via Tailscale Serve.

## How to Stop

```bash
bash $HEX_DIR/.hex/skills/secret-intake/scripts/stop.sh
```

## What Happens on Submit

1. Each institution's env vars are written to `.hex/secrets/{institution}.env`
2. Key files (PEM, JSON, p12, etc.) are written to `.hex/secrets/{institution}-{filename}`
3. All files get `chmod 600`
4. If an `.env` file already exists for that institution, new keys are *merged* (existing keys preserved, matching keys updated)
5. `sync-secrets.sh` runs automatically — propagates to `launchctl setenv` + cc-connect plist + daemon restart

## Security Properties

- Server binds to `0.0.0.0` but only reachable via Tailscale (no public exposure)
- Zero HTTP logging — `log_message` is a no-op
- `Cache-Control: no-store` on every response
- Secret values never appear in terminal output, logs, or transcripts
- All secret files are gitignored (`*.env` in `.gitignore`)
- Form data sent via POST body, never URL params

## File Layout

```
.hex/skills/secret-intake/
├── SKILL.md              ← this file
└── scripts/
    ├── server.py         ← the intake server
    ├── start.sh          ← launch server in background
    └── stop.sh           ← kill server
```

## Storage Convention

| Type | Path | Example |
|------|------|---------|
| Env vars | `.hex/secrets/{institution}.env` | `.hex/secrets/alpaca.env` |
| Key files | `.hex/secrets/{institution}-{filename}` | `.hex/secrets/coinbase-api-key.pem` |

## Routing

The intake server is plain HTTP on `:9877`. hex-router (`:8880`) proxies `/secrets` → `:9877` with prefix stripping. Tailscale Serve fronts hex-router on `:443` with TLS.

```
Browser → https://<your-tailscale-hostname>/secrets
       → Tailscale Serve (TLS termination)
       → hex-router :8880 /secrets → strip prefix → :9877 /
       → secret-intake server (plain HTTP)
```

## Configuration

| Env var | Default | What it does |
|---------|---------|-------------|
| `PORT` | `9877` | Server listen port |
| `HEX_DIR` | `$HEX_DIR` | Hex root directory |
