# Style feedback

`system/scripts/gdd-style-gate.py` provides local, advisory format feedback.

## Behavior

- The hook reads the current assistant message or a bounded transcript tail.
- It reports only objective format observations: dash punctuation, header shape, and standalone bold labels.
- Code blocks, quoted evidence, paths, URLs, and structured JSON are exempt.
- It prints findings to stderr and writes metadata-only `advisory` rows to `GDD_GATE_LOG`.
- It never calls a model, reads a credential, rewrites a reply, emits a blocking decision, or returns a style failure exit.
- Malformed input, unavailable transcripts, oversized input, and log failures remain visible and fail open.
- Retry payloads marked `stop_hook_active` are ignored.

## Installation

Install the script under the instance `.hex/scripts/` directory. Keep any existing `.hex/bin/gdd-style-gate.py` caller path as a relative compatibility symlink to that file. Set `HEX_DIR` when the install location cannot be found from the script path. Set `GDD_GATE_LOG` for test or alternate telemetry output.

The hook does not enforce permissions or command safety. Runtime hooks must keep those controls separate from presentation feedback.

## Verification

```sh
python3 -I -B tests/test_gdd_style_gate.py
```

The tests use standard-library mocks and do not call a model, read secrets, or run privileged commands.

The `advisory hook contract tests` CI job runs the same suite. It tests the
hook's nonblocking behavior, not the writing style of worker answers.
