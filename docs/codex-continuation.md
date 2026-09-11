# Codex continuation delivery

`codex_continuation.py` queues one identified message on an existing Codex
thread. Use it when the current thread owns a follow-up action that must start
after the active turn finishes, including when the client disconnects.

The helper is a delivery guard. It is not a scheduler. Keep real task
dependencies in the BOI task graph.

## Usage

Put the complete continuation instruction in a regular UTF-8 file. Give every
logical action a stable ID and one designated owner.

```bash
python3 "$HEX_DIR/.hex/scripts/codex_continuation.py" \
  --action-id source-review-20260909 \
  --owner root \
  --thread-id THREAD_ID \
  --message-file /path/to/continuation.txt
```

The default endpoint is the local Codex app-server control socket under
`$CODEX_HOME` when that variable is set, or under `~/.codex` otherwise.
Override it with `--socket /absolute/path/to/socket` when the local runtime
exposes a different Unix socket. The helper does not use network hosts,
authentication, or Codex settings.

The default state directory is
`$HEX_DIR/.hex/state/codex-continuation/`. Use `--state-dir` only for an
isolated instance or a test. State files are runtime receipts, not config.

## Delivery contract

The helper:

- Binds the action ID to the owner, thread, endpoint, and message hash.
- Adds a stable marker to the submitted user message.
- Takes a per-action process lock.
- Writes intent to a durable journal before the send boundary.
- Checks the full queued-submission list and paginated thread-item history
  before a first send.
- Keeps the lock through submission and a fresh read-back.
- Returns an existing durable receipt on a matching repeat.
- Rejects an action ID reused with different intent.

A successful JSON result records whether the marker was found in the queue or
in history when the receipt was created. This is historical delivery evidence.
It is not current queue or execution status, and it does not prove that the
requested work completed. Repeating the same command returns that immutable
receipt without contacting Codex.

## Uncertain delivery

Exit code `3` means delivery is uncertain. This includes a timeout or malformed,
oversized, incomplete, or unbounded server response that prevents a complete
queue and history check.

`--timeout` bounds the complete operation. It includes lock acquisition,
connection setup, queue and history scans, submission, and read-back.
Queue scans are limited to `100` pages. History scans are limited to `1000`
pages. Reaching either bound returns uncertain without adding a message. Both
limits remain subordinate to the complete operation's absolute timeout.

The helper never blindly retries an ambiguous send. Run the same command again
to reconcile the stable marker. If complete live state contains the action, the
helper records and returns the receipt. If the action remains absent after an
ambiguous send, the command stays loud and does not submit a replacement.

Exit code `2` reports invalid input, conflicting intent, or another definite
failure. Errors are JSON objects on stderr. Successful receipts are JSON
objects on stdout.

## Limits

This guard reduces duplicate delivery from ordinary retries and concurrent
callers. Codex queue addition is not a transactional exactly-once mechanism.
The downstream action must remain safe to retry, and BOI continues to own task
dependencies, retries, and completion evidence.

The existing Codex server starts a queued successor after the active turn ends.
A server restart can preserve that queue without starting it. Explicitly
resuming the owning thread can start the stored successor, while the predecessor
remains interrupted. This helper does not resume threads or provide restart
scheduling.
