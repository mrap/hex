# REPLACE_ME Integration Runbook

**Integration:** REPLACE_ME
**Tier:** standard
**Owner:** mike
**Bundle:** `integrations/REPLACE_ME/`

---

## Quick Status

```bash
hex-integration status REPLACE_ME
hex-integration probe REPLACE_ME
```

---

## Symptom: [describe symptom]

**Likely causes:**
1. ...

**Diagnostics:**
```bash
# ...
```

**Fix:**
```bash
# ...
```

---

## Setup from Scratch

```bash
# 1. Obtain credentials from provider
# 2. Copy and fill secrets
cp integrations/REPLACE_ME/secret.env.example .hex/secrets/REPLACE_ME.env
# Edit .hex/secrets/REPLACE_ME.env
chmod 600 .hex/secrets/REPLACE_ME.env

# 3. Install
hex-integration install REPLACE_ME

# 4. Verify
hex-integration probe REPLACE_ME
```

---

## Last Known Good

```bash
cat "$HEX_ROOT/projects/integrations/_state/REPLACE_ME.json"
```

---

## Escalation

Channel: `#integrations`
