# REPLACE_ME

One-line description of this integration.

**Tier:** standard
**Auth:** [describe auth mechanism]

## Status

```bash
hex-integration status REPLACE_ME
hex-integration probe REPLACE_ME
```

## Bundle layout

```
integrations/REPLACE_ME/
├── integration.yaml       manifest
├── probe.sh               health check
├── secret.env.example     secrets schema (copy to .hex/secrets/REPLACE_ME.env)
├── runbook.md             operational runbook
├── README.md              this file
├── maintenance/           (if applicable)
│   └── rotate.sh
├── events/                (if applicable)
│   └── <policy>.yaml
└── tests/
    ├── run.sh
    └── probe.test.sh
```

## Secrets

| Variable | Required | Description |
|---|---|---|
| `REPLACE_ME_API_KEY` | ✅ | API key from provider |

## Setup

```bash
hex-integration install REPLACE_ME
```
