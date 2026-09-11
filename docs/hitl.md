<!--
verified-against: feature/hex-hitl
source-paths: system/harness/src/hitl/{mod,store,policy,transport}.rs, system/harness/src/main.rs (HitlCommands, run_hitl), system/templates/launchd/com.hex.hitl-nudge.plist
-->
# HITL — the pending-human-action queue

`hex hitl` is a queue of actions only a human can do — sign a document, pass KYC,
pay an invoice, grant a permission. Agents **file** an item when they hit such a
block and keep working; the system pings you over iMessage by urgency, rolls
everything into one daily digest, and you **close** items from the CLI when done.

It is a generic hex primitive — no daemon, no database. The whole queue is plain
files under `$HEX_DIR/.hex/hitl/`, so it survives restarts and is inspectable by
hand:

```text
$HEX_DIR/.hex/hitl/
  items/<id>.toml   one file per item (ids are sequential, 1-based)
  log.jsonl         append-only: every state transition, every ping sent
  config.toml       mode / imessage_handle / digest_hour / quiet hours / cap
  state/            stamp files (digest-sent-YYYY-MM-DD, ping counters)
```

## CLI reference

```bash
# File an item. Prints the new id. Sends an immediate ping when policy says so.
hex hitl add --project <p> --title <t> --priority P1|P2|P3 \
    [--deadline YYYY-MM-DD] [--est <minutes>] [--depends-on <id,id>] [--body <text>]
#   --body -   reads the markdown body (exact steps + links) from stdin

hex hitl list [--all]          # open + snoozed by default; --all includes closed
hex hitl show <id>             # full item, including its markdown body

hex hitl done <id> [--note <s>]     # close as done
hex hitl skip <id> [--note <s>]     # close as skipped
hex hitl snooze <id> --until YYYY-MM-DD   # silent until that date, then open again

hex hitl nudge                 # launchd entry point (see below): send due pings +
                               #   the daily digest if it's digest_hour and not sent yet
hex hitl digest                # compose + send the digest unconditionally, now
```

### Item schema (`items/<id>.toml`)

| field | type | notes |
| --- | --- | --- |
| `id` | u64 | sequential, 1-based (next = max existing + 1) |
| `title` | string | short summary |
| `project` | string | grouping key in the digest |
| `body` | string | markdown — exact steps + links |
| `priority` | `P1`\|`P2`\|`P3` | drives ping cadence (see policy) |
| `deadline` | date? | `YYYY-MM-DD`; enables P2 escalation |
| `est_minutes` | u32? | shown in list/digest |
| `depends_on` | [id] | open/snoozed deps ⇒ item is "blocked" |
| `status` | `open`\|`snoozed`\|`done`\|`skipped` | |
| `created` | RFC3339 | set on `add` |
| `snooze_until` | date? | set by `snooze` |
| `last_pinged` | RFC3339? | drives P1 24h re-ping |
| `closed_at` | RFC3339? | set by `done`/`skip` |
| `note` | string? | optional close/skip note |

## Ping policy

The decision logic (`policy::pings_due`) is a pure function — given the items,
config, the current time, how many individual pings have already gone out today,
and whether the digest has been sent — it returns the list of pings to send. No
I/O, no wall clock inside; it is table-driven unit-tested.

| Priority | On file | Re-ping | Digest |
| --- | --- | --- | --- |
| `P1` | ping | every 24h while open (`last_pinged`) | yes |
| `P2` | ping | only when a `deadline` is set: extra pings crossing T-48h and T-24h | yes |
| `P3` | — | never pinged individually | yes (digest only) |

Additional rules:

- **Blocked by dependency** — an item with any open/snoozed id in `depends_on` is
  never pinged individually; it shows as **blocked** in the digest.
- **Snoozed** — silent: no pings, excluded from the digest, until `snooze_until`
  passes, after which it is treated as open again.
- **Quiet hours** `[quiet_start, quiet_end)` — no individual pings fire at night;
  a ping that becomes due then fires at the next `nudge` after `quiet_end`. The
  digest is not special-cased — `digest_hour` is simply expected to sit outside
  quiet hours.
- **Daily cap** — at most `max_pings_per_day` individual pings per calendar day
  (the digest is excluded). Overflow waits for the next day, highest priority
  first (P1 before P2), then oldest first.
- **`mode = "batched"`** — suppresses on-file and re-pings for P2/P3 entirely;
  P1 rules and the digest still apply.

## Digest

One message: open items grouped by project, priority-sorted within each group,
`deadline` and `est_minutes` shown when present, blocked items flagged. The header
line carries the total open count and summed estimate, e.g.
`HITL: 4 open, ~35 min`. An empty queue sends nothing.

## Transport

`hex hitl` sends over iMessage via `osascript` using the modern Messages pattern
(`send <text> to participant <handle> of (1st account whose service type =
iMessage)`). The handle comes from `config.imessage_handle`. If the handle is
unset or the send fails, it falls back to `crate::alert::notify` with the same
text and logs the degradation. Every send attempt — success, fallback, or
failure — is recorded in `log.jsonl` and telemetry. No message content is ever
read back; this is a ping-only transport.

## Configuration (`config.toml`)

A missing file means all defaults. A malformed file is a **loud error**, never a
silent fallback to defaults.

| key | default | meaning |
| --- | --- | --- |
| `mode` | `immediate` | `immediate` pings on file; `batched` suppresses P2/P3 pings |
| `imessage_handle` | *(unset)* | iMessage recipient; unset ⇒ `alert::notify` fallback |
| `digest_hour` | `9` | hour (0–23) the daily digest is sent |
| `quiet_start` | `22` | quiet-hours window start (inclusive) |
| `quiet_end` | `8` | quiet-hours window end (exclusive) |
| `max_pings_per_day` | `3` | individual ping cap per day (digest excluded) |

Example:

```toml
mode = "immediate"
imessage_handle = "+15551234567"
digest_hour = 9
quiet_start = 22
quiet_end = 8
max_pings_per_day = 3
```

## Scheduling (launchd)

`hex hitl nudge` is the hourly driver. The template at
`system/templates/launchd/com.hex.hitl-nudge.plist` describes the historical
legacy-only registration, with substituted `HEXBIN`, `HEXDIR` and log paths.
It is not a qualified managed macOS service installer.

There is no qualified automatic managed HITL-nudge installation path yet. Do
not render an arbitrary executable into that template and bootstrap it on a
managed installation. The common signer does not itself qualify every service
that invokes Hex. See the [macOS build standard](macos-build-standard.md) for
covered callers and remaining service limits.

The job runs `hex hitl nudge` every hour (`StartInterval` 3600): it sends the
pings due now and, when the current hour equals `digest_hour` and the digest has
not yet gone out today, sends the daily digest.
