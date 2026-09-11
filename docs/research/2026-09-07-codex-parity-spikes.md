# Codex parity spikes (Phase 0) ledger

Dated research entries for the Codex parity Phase 0 spikes. One entry per spike in the form: Question / Method (exact commands) / Result / Decision or Follow-up. Absolute temp paths are redacted to `<TMP>`.

## S0.5 Skill discovery and instruction-file fallbacks

Question. Does Codex 0.153.4 discover project skills placed under `.agents/skills/` (including a symlinked skill dir that carries extra hex frontmatter keys)? Does `project_doc_fallback_filenames = ["CLAUDE.md"]` make Codex read a `CLAUDE.md` when no `AGENTS.md` is present, and is the key accepted under `--strict-config`? Does Claude Code follow a symlinked `CLAUDE.md`?

Method. All Codex probes ran in a temp project with a temp `CODEX_HOME` seeded only with a copy of `~/.codex/auth.json`.

```
T=$(mktemp -d) && mkdir -p "$T/home"
cp ~/.codex/auth.json "$T/home/auth.json" && chmod 600 "$T/home/auth.json"

# (a) project skill via symlink under .agents/skills
mkdir -p "$T/proj" && (cd "$T/proj" && git init -q)
cp -R system/skills/hex-doctor "$T/skillcopy/hex-doctor"
mkdir -p "$T/proj/.agents/skills"
ln -s "$T/skillcopy/hex-doctor" "$T/proj/.agents/skills/hex-doctor"
cd "$T/proj" && CODEX_HOME="$T/home" codex debug prompt-input "hi"
# live enumeration (records exec-path stderr for frontmatter warnings):
cd "$T/proj" && CODEX_HOME="$T/home" codex exec --output-schema "$T/a-schema.json" -o "$T/a-out.json" \
  -s read-only -m gpt-5.4-mini "List the exact names of every skill available to you." < /dev/null

# (b) CLAUDE.md via project_doc_fallback_filenames, only CLAUDE.md present
mkdir -p "$T/projb" && (cd "$T/projb" && git init -q)
printf '# Project Notes\n\nThe project codeword is FALLBACKWORD-9931.\n' > "$T/projb/CLAUDE.md"
printf 'project_doc_fallback_filenames = ["CLAUDE.md"]\n' > "$T/home/config.toml"
cd "$T/projb" && CODEX_HOME="$T/home" codex debug prompt-input "What codeword is in my project instructions?"
# strict-config acceptance plus live echo (only codex exec supports --strict-config):
cd "$T/projb" && CODEX_HOME="$T/home" codex exec --strict-config -s read-only -m gpt-5.4-mini \
  "What codeword is in my project instructions? Answer with only the codeword." < /dev/null

# (c) Claude Code symlinked CLAUDE.md (disambiguating layout: distinct codewords)
mkdir -p "$T/projc"
printf '# Agents doc\n\nThe agents codeword is ALPHA-AGENTS-4471.\n' > "$T/projc/AGENTS.md"
printf '# Real instructions\n\nThe symlink codeword is BRAVO-SYMLINK-8802.\n' > "$T/projc/INSTRUCTIONS-REAL.md"
ln -s "INSTRUCTIONS-REAL.md" "$T/projc/CLAUDE.md"
CT=$(mktemp -d)  # temp CLAUDE_CONFIG_DIR so ~/.claude.json is not mutated
cd "$T/projc" && CLAUDE_CONFIG_DIR="$CT" claude -p --model sonnet --output-format json \
  "List every codeword that appears in your project instructions."
```

<!-- merged from task branch hook-trust-hash (fence repair 2026-09-07): duplicate h1 dropped, all content kept -->

Dated research ledger for the codex-parity Phase 0 spikes. Each entry:
Question / Method (exact commands) / Result / Decision or Follow-up. Plain
language, short sentences, no em dashes.

## S0.3 Hook trust hash reproduction

Question. Can the hex harness recompute Codex's per-hook `trusted_hash` exactly,
so it can tell whether a hook it is about to install is already trusted in the
user's `~/.codex/config.toml`?

Method (exact commands).

```
# Pin the source. Tag rust-v0.153.4 is annotated; dereference to the commit.
curl -sSL "https://api.github.com/repos/openai/codex/git/ref/tags/rust-v0.153.4"        # tag obj 042fb41b7c813ac7999105e886b2b7aa715b5081
curl -sSL "https://api.github.com/repos/openai/codex/git/tags/042fb41b7c813ac7999105e886b2b7aa715b5081"   # commit 3d2ee51ca2d5db578f328aa75e20aa22c0197c9a

# Fetch the algorithm and type definitions.
curl -sSL "https://raw.githubusercontent.com/openai/codex/rust-v0.153.4/codex-rs/hooks/src/engine/discovery.rs"
curl -sSL "https://raw.githubusercontent.com/openai/codex/rust-v0.153.4/codex-rs/hooks/src/config_rules.rs"
curl -sSL "https://raw.githubusercontent.com/openai/codex/rust-v0.153.4/codex-rs/config/src/fingerprint.rs"
curl -sSL "https://raw.githubusercontent.com/openai/codex/rust-v0.153.4/codex-rs/config/src/hook_config.rs"
curl -sSL "https://raw.githubusercontent.com/openai/codex/rust-v0.153.4/codex-rs/hooks/src/lib.rs"
curl -sSL "https://raw.githubusercontent.com/openai/codex/rust-v0.153.4/codex-rs/hooks/src/events/session_end.rs"
curl -sSL "https://raw.githubusercontent.com/openai/codex/rust-v0.153.4/codex-rs/hooks/src/output_spill.rs"
curl -sSL "https://raw.githubusercontent.com/openai/codex/rust-v0.153.4/codex-rs/hooks/src/events/common.rs"
curl -sSL "https://raw.githubusercontent.com/openai/codex/rust-v0.153.4/codex-rs/Cargo.lock"

# Derive the expected hash independently (mirrors canonical_json + serde_json::to_vec).
python3 -c "import hashlib,json; s=json.dumps({'event_name':'session_start','hooks':[{'type':'command','command':'/bin/echo hi','timeout':600,'async':False}]},separators=(',',':'),sort_keys=True); print('sha256:'+hashlib.sha256(s.encode()).hexdigest())"

# Implement and test.
export PATH="/opt/homebrew/bin:$PATH"
cargo test --manifest-path system/harness/Cargo.toml codex_hook_hash
```

Result.

- (a) Codex 0.153.4 surfaces skills in a `<skills_instructions>` developer message with two roots: `r0` = `$CODEX_HOME/skills/.system` (built-in) and `r1` = `<project>/.agents/skills` (project). The symlinked `hex-doctor` skill is surfaced under `r1`, so Codex follows the symlink. Only `name` and `description` frontmatter are rendered; the extra `version: 1.0.0` key and the `<!-- # sync-safe -->` comment are dropped with no warning. A fresh `CODEX_HOME` is seeded with system skills (`imagegen`, `openai-docs`, `plugin-creator`, `review-agent`, `skill-creator`, `skill-installer` plus a `.codex-system-skills.marker`); six are surfaced to the model but `review-agent` is not. The live `codex exec --output-schema` call returned (exit 0, schema-conformant) `imagegen`, `openai-docs`, `plugin-creator`, `skill-creator`, `skill-installer`, `hex-doctor`, plus two plugin-namespaced entries (`deep-research-work:deep-research`, `plugin-management:plugin-management`) that are a model self-report artifact and are not in the authoritative `<skills_instructions>` block. Exec stderr logged `codex_skills_extension::host_service: skills cache cleared` and no frontmatter or unknown-field warning.
- (b) With `project_doc_fallback_filenames = ["CLAUDE.md"]` in the temp user config and only `CLAUDE.md` present, the model-visible prompt gains an `# AGENTS.md instructions for <TMP>/projb` block wrapping the `CLAUDE.md` body (codeword `FALLBACKWORD-9931`). Removing the key drops the block (control: zero matches). The live `codex exec --strict-config` call exited 0 (the key is recognized in 0.153.4, not a STOP condition) and the model answered `FALLBACKWORD-9931`. Note: `codex debug prompt-input` does not accept `--strict-config`; only `codex exec` gates config that way.
- (c) The live Claude call (exit 0, `is_error: false`, one turn) answered `The codeword in my project instructions is: BRAVO-SYMLINK-8802`. Only `BRAVO-SYMLINK-8802` (the symlink-target codeword) appeared; `ALPHA-AGENTS-4471` (the `AGENTS.md` codeword) did not. Claude Code 2.1.263 follows a symlinked `CLAUDE.md` and reads its target. First attempt with an empty temp `CLAUDE_CONFIG_DIR` returned `Not logged in`, so the dir was seeded with copies of `~/.claude/.credentials.json` and `~/.claude.json`; the real `~/.claude.json` mtime was unchanged before and after (isolated writes). The disambiguating layout deviates from the brief's `CLAUDE.md -> AGENTS.md` on purpose so the returned codeword pins down which read path fired.

Decision or Follow-up.

- Codex parity plan: ship hex skills to Codex as `.agents/skills/<name>/SKILL.md` (symlinks are fine); keep only `name` and `description` in frontmatter that Codex must honor (extra hex keys are ignored, not rejected). To have Codex read a hex `CLAUDE.md`, set `project_doc_fallback_filenames = ["CLAUDE.md"]` in the user config; it survives `--strict-config`.
- Claude parity: a `CLAUDE.md -> AGENTS.md` symlink lets a single instruction file feed Claude Code, since the symlink is followed. Follow-up: characterize whether Claude Code 2.1.263 reads `AGENTS.md` at all when a `CLAUDE.md` is present (this probe saw `CLAUDE.md` win and `AGENTS.md` not contribute); that precedence question is out of scope for S0.5 and belongs to a later phase.
- Isolation follow-up recorded: `CLAUDE_CONFIG_DIR` redirects both config and credential lookup, so any headless Claude probe must seed credentials into the temp config dir.
## S0.2 Codex hook payloads per event

Question: What JSON does Codex 0.153.4 send to each hook event under `codex exec`,
and which events are reachable headless? Do both the user and project hook layers
fire? Do HookStarted/HookCompleted appear in the `--json` stream?

Method: Built a temp CODEX_HOME and temp git project. Installed one silent dump
hook (writes stdin to a per-probe file, exits 0) for all 12 events in BOTH
`$CODEX_HOME/hooks.json` and `<proj>/.codex/hooks.json`. Trusted the project via
`[projects."<proj>"] trust_level = "trusted"` in the temp user config. Ran hooks
headless with `--dangerously-bypass-hook-trust`. Probes:

```
# Probe A: shell tool run (SessionStart, UserPromptSubmit, PreToolUse, PostToolUse, Stop, SessionEnd)
CODEX_HOME="$T/home" codex exec --json -m gpt-5.4-mini -s workspace-write \
  --dangerously-bypass-hook-trust -C "$T/proj" \
  "Run the shell command: echo hello-from-probe-A . ... reply ... DONE and stop." < /dev/null

# Probe B: apply_patch edit from a subdirectory (-C points at proj/sub)
CODEX_HOME="$T/home" codex exec --json -m gpt-5.4-mini -s workspace-write \
  --dangerously-bypass-hook-trust -C "$T/proj/sub" \
  "Edit the file target.txt ... Use the apply_patch tool ..." < /dev/null

# Probe C/C2: spawn one subagent (--enable multi_agent); C2 makes the subagent run a shell command
CODEX_HOME="$T/home" codex exec --json -m gpt-5.4-mini -s workspace-write \
  --enable multi_agent --dangerously-bypass-hook-trust -C "$T/proj" \
  "Launch one subagent ... compute 2+2 ..." < /dev/null

# Probe D: force compaction with a small context window
CODEX_HOME="$T/home" codex exec --json -m gpt-5.4-mini -s workspace-write \
  -c model_context_window=16000 --dangerously-bypass-hook-trust -C "$T/proj" \
  "Do the following one command at a time ... seq 1 400 ... seq 1601 2000 ... DONE." < /dev/null
```

Result:

- Both hook layers fire. Every event fired twice per run, once per layer.
- Reachable headless: SessionStart, UserPromptSubmit, PreToolUse, PostToolUse,
  Stop, SessionEnd, SubagentStart, SubagentStop, PreCompact, PostCompact.
- Not reached by the invocations probed here: PermissionRequest (all probes ran
  `approval_policy = never`; the `codex exec --approve-for-me` path routes approval
  requests and was not probed) and Interrupt (needs an interrupt signal; not probed
  via other paths).
- SessionStart `source`: `"startup"` normally, `"compact"` after an auto-compact.
- PreToolUse/PostToolUse `tool_name` is `"Bash"` for the shell tool and
  `"apply_patch"` for patches; subagent orchestration uses `spawn_agent` and
  `multi_agent_v1wait_agent`.
- apply_patch paths are cwd-relative. With `-C proj/sub`, the payload `cwd` is the
  subdirectory and the patch names the file as bare `target.txt`, not absolute and
  not git-root-relative.
- Stop has `stop_hook_active` and `last_assistant_message`. SessionEnd has
  `reason` (`"other"`) and its timeout is clamped to 3s.
- Subagents: UserPromptSubmit fires for the subagent prompt; PreToolUse and
  PostToolUse fire for the subagent's own tool calls; Stop fires only for the main
  agent, while the subagent ends with SubagentStop (which carries `agent_id`,
  `agent_type`, `agent_transcript_path`, `last_assistant_message`).
- PreCompact/PostCompact carry `trigger` (`"auto"`).
- HookStarted/HookCompleted do NOT appear in the `--json` stream. Stream types are
  `thread.started`, `turn.started`, `turn.completed`, `item.started`,
  `item.completed`. Hook notices appear only as `item.completed` error records.

Fixtures committed: tests/fixtures/codex/hooks/{session-start, session-start-compact,
user-prompt-submit, pre-tool-use-shell, pre-tool-use-apply-patch, post-tool-use,
stop, session-end, subagent-start, subagent-stop, pre-compact, post-compact}.json
(scrubbed). See docs/runtimes.md "## Hook payloads".

Decision or Follow-up: hex must not rely on the `--json` stream for hook
observability; watch hook process side effects instead. Re-verify `permission_mode`
under a trusted-hash run (not bypass) once T3 lands the hook hash, since this spike
only exercised the exec `approval_policy = never` path.

## S0.11 Interactive TUI turn (manual, cannot run headless)

Question: Does one interactive TUI turn produce the same hook sequence as
`codex exec`, and what does an interactive PermissionRequest payload look like?

Method: Not runnable in this spike. Interactive `codex` is forbidden headless and
PermissionRequest never fires under exec (`approval_policy = never`).

Result: Deferred to a manual step for Mike.

Decision or Follow-up: Manual check for Mike. Start `codex` (interactive TUI) in a
project that has the dump hook installed. Submit one prompt that triggers a shell
tool call and, if possible, one command that needs approval. Confirm the hook log
dir shows SessionStart, UserPromptSubmit, PreToolUse, PostToolUse, Stop, and any
PermissionRequest. Record the PermissionRequest payload shape; it is the one event
this headless spike could not capture.

## S0.12 apply_patch path form and subagent hook coverage

Question: Are apply_patch paths in the hook payload absolute or cwd-relative when
the run starts in a subdirectory? Do UserPromptSubmit, PreToolUse, and Stop fire
for subagents?

Method: Covered by S0.2 Probe B (subdirectory apply_patch) and Probes C/C2
(subagent). See the commands above.

Result:

- apply_patch paths are cwd-relative. Running with `-C proj/sub`, the PreToolUse
  payload `cwd` was the subdirectory and the patch body read
  `*** Update File: target.txt` (bare, relative to cwd), not an absolute path and
  not relative to the git root.
- UserPromptSubmit fires for the subagent's prompt (observed the subagent prompt
  `"Compute 2+2 ..."` as its own UserPromptSubmit).
- PreToolUse and PostToolUse fire for the subagent's own tool calls (observed the
  subagent's `echo i-am-the-subagent` Bash call).
- Stop does NOT fire for the subagent; the subagent's terminal hook is
  SubagentStop.

Decision or Follow-up: hex hook adapters that key on paths must treat apply_patch
paths as cwd-relative and resolve them against the payload `cwd`. Hook logic that
should also cover subagents can rely on UserPromptSubmit / PreToolUse / PostToolUse
firing inside subagents, but must treat SubagentStop (not Stop) as the subagent
terminal signal.

---

<!-- merged from task branch exec-envelope (operator conflict resolution 2026-09-07): duplicate h1 dropped, all content kept -->


Dated research ledger for the codex-parity Phase 0 spikes. One section per spike.
Each section states the Question, the Method with exact commands, the Result, and
the Decision or Follow-up. All probes ran against codex-cli 0.153.4 on macOS with
ChatGPT auth, using a throwaway `CODEX_HOME` and a git initialized temp project
(setup documented in docs/runtimes.md).

## S0.1 codex exec --json envelope plus --output-schema and stdin

Question. What events does `codex exec --json` emit for a one turn run, does
`--output-schema` enforce strict conformance or fall back, and does a 170 KB
prompt on stdin (with the positional `-`) reach the model?

Method. Temp CODEX_HOME with a copied auth.json, git initialized temp project.

```bash
# Envelope
CODEX_HOME="$T/home" codex exec --json -m gpt-5.4-mini -C "$PROJ" -s read-only \
  "Reply with exactly the single word: hello" < /dev/null
# Output schema: flat (3 required, additionalProperties false), nested, top-level oneOf
CODEX_HOME="$T/home" codex exec --json -m gpt-5.4-mini -C "$PROJ" -s read-only \
  --output-schema "$T/schema-flat.json"   -o "$T/result-flat.json"   "...demo, count 3, not done" < /dev/null
CODEX_HOME="$T/home" codex exec --json -m gpt-5.4-mini -C "$PROJ" -s read-only \
  --output-schema "$T/schema-nested.json" -o "$T/result-nested.json" "...meta.count 2, meta.tags [a,b]" < /dev/null
CODEX_HOME="$T/home" codex exec --json -m gpt-5.4-mini -C "$PROJ" -s read-only \
  --output-schema "$T/schema-oneof.json"  -o "$T/result-oneof.json"  "...kind number, value 7" < /dev/null
# Stdin: 174206 byte prompt, codeword at the end; three cases
CODEX_HOME="$T/home" codex exec --json -m gpt-5.4-mini -C "$PROJ" -s read-only - < "$T/big.txt"   # positional dash
CODEX_HOME="$T/home" codex exec --json -m gpt-5.4-mini -C "$PROJ" -s read-only   < "$T/big.txt"   # no positional
CODEX_HOME="$T/home" codex exec --json -m gpt-5.4-mini -C "$PROJ" -s read-only "...codeword near end..." < "$T/big.txt"
```

Result.

- Envelope: four JSONL events in order. `thread.started` (carries `thread_id`),
  `turn.started` (empty), `item.completed` (`item.type` `agent_message`, `text`
  `hello`), `turn.completed` (`usage` with `input_tokens`, `cached_input_tokens`,
  `cache_write_input_tokens`, `output_tokens`, `reasoning_output_tokens`).
  Human readable output and all tracing go to stderr. Captured in
  `tests/fixtures/codex/exec-envelope.jsonl` (thread_id scrubbed).
- Output schema: strict, no fallback. Codex forwards the schema to the API as
  `response_format` named `codex_output_schema`. Flat schema conforms exactly
  (exit 0, `{"title":"demo","count":3,"done":false}`). Nested object conforms
  exactly (exit 0, `{"title":"demo","meta":{"count":2,"tags":["a","b"]}}`). Top
  level `oneOf` is rejected by the API: `invalid_request_error`, code
  `invalid_json_schema`, `'oneOf' is not permitted`, status 400. The stream emits
  `error` and `turn.failed`, no `-o` file is written, and the process exits 1.
  Fixtures: `exec-output-schema.json` and `.result.json` (flat),
  `exec-output-schema.nested.json` and `.nested.result.json`,
  `exec-output-schema.oneof.json` and `.oneof.error.json`.
- Stdin: the codeword `PLATINUM-WALRUS-42` was echoed in all three cases, so the
  full 170 KB prompt reaches the model. The positional `-` is not required: with
  no positional and piped stdin the codeword is still echoed. With a positional
  prompt plus piped stdin, the pipe is appended as a `<stdin>` block (stderr logs
  `Reading additional input from stdin...`). One dash run first returned a safety
  refusal that still named the hidden codeword (proving it read the tail); an
  immediate re run echoed it. Refusal is model nondeterminism, not a `-`
  behavior difference.

Decision or Follow-up. Envelope shape and strict schema behavior are pinned for
the Phase 2 and 3 exec gates. Parity code must treat `turn.failed` as a non zero
exit and must not expect Codex to soften an API rejected schema. Feed prompts on
stdin with `/dev/null` closed on non prompt runs.

## S0.13 shell_environment_policy

Question. Does the default `shell_environment_policy` strip environment variable
names containing KEY, SECRET, or TOKEN from the tool shell? Two reads of the
config reference disagreed.

Method. Export four vars in the launching shell, ask the model to run a shell
command listing the matching names, under the default policy and under
`inherit=all`.

```bash
MY_TEST_KEY=1 MY_TEST_SECRET=1 MY_TEST_TOKEN=1 PLAIN=1 \
CODEX_HOME="$T/home" codex exec --json -m gpt-5.4-mini -C "$PROJ" -s workspace-write \
  "Run this exact shell command and report its full stdout verbatim (names only): env | grep -E 'KEY|SECRET|TOKEN' | cut -d= -f1 | sort" < /dev/null
# repeat with: -c shell_environment_policy.inherit=all
```

Result. No stripping. The model made a real `command_execution` tool call. Under
the default policy the tool shell saw 5 matching names: `CLAUDE_CODE_MESSAGING_TOKEN`,
`MY_TEST_KEY`, `MY_TEST_SECRET`, `MY_TEST_TOKEN`, `STARSHIP_SESSION_KEY`. Under
`inherit=all` it saw 4: the same set minus `STARSHIP_SESSION_KEY`. The load
bearing inference: `MY_TEST_KEY`, `MY_TEST_SECRET`, and `MY_TEST_TOKEN` are ad
hoc exports that no shell profile sets, so their presence in the tool shell under
the default policy can only be parent environment inheritance. That proves there
is no default name based stripping. The 5 versus 4 difference does not weaken
that inference. Its mechanism: the command was executed as `/bin/zsh -lc "..."`
in the default run and `/bin/zsh -c "..."` in the `inherit=all` run. The login
form re sources the user profile, which sets `STARSHIP_SESSION_KEY` fresh; the
non login form does not. That variable came from the profile, not the parent.
What this probe does not settle is why the invocation form changed between the
two runs (whether the `shell_environment_policy.inherit` key governs the login
shell form, or the form varied for an unrelated reason). See the follow-up below.

Decision or Follow-up. The default does not strip secret named vars, so the
plan's conditional `ignore_default_excludes` decision in runtime.toml is not
needed. The inverse risk is real: Codex tool shells inherit every secret in the
launching process environment. hex must scrub or drop secrets before launching
`codex exec` rather than rely on a default policy. Record this in
docs/runtimes.md (done).

Open follow-up. Confirm whether `shell_environment_policy.inherit` controls
whether the tool shell is a login shell (`-lc`) or not (`-c`). Cheap probe for a
later phase: ask the model, under each policy, to run `echo "$0 $-"` and report
whether the shell was login. Phase 2 and 3 need this because a login tool shell
re sources the user profile and can pull in profile only variables.

## S0.14 codex debug prompt-input

Question. What is the JSON shape of `codex debug prompt-input`, and does a 40 KB
AGENTS.md reach the model in full when `project_doc_max_bytes` is raised?

Method. Temp project with a 40 KB AGENTS.md (40975 bytes) carrying an early
marker and a tail marker past 39 KB, and `project_doc_max_bytes = 131072` in the
project `.codex/config.toml`. The subcommand has no `-C` flag, so run it from
inside the project. It makes no model call.

```bash
( cd "$PROJ" && CODEX_HOME="$T/home" codex debug prompt-input \
    "What are the codewords in my project instructions?" )
# CLI override alternative:
( cd "$PROJ" && CODEX_HOME="$T/home" codex debug prompt-input -c project_doc_max_bytes=131072 "..." )
```

Result. Output is a JSON array of messages, each
`{type:"message", id, role, content, internal_chat_message_metadata_passthrough}`
with `content` a list of text parts. A one turn setup emitted 5 messages: 3
`developer`, then 2 `user`. The first `user` message holds three parts
(`content_item_kinds` `plugins.recommendations`, `agents_md.instructions`,
`environments.environment_context`): the AGENTS.md is its own
`agents_md.instructions` part sitting alongside a distinct `<environment_context>`
part, not inside it. The second `user` message is the prompt. Shape pinned,
scrubbed, strings over 200 chars truncated, in
`tests/fixtures/codex/prompt-input.json`. The AGENTS.md reached the model in full
only when the project layer was actually applied: with the trust entry keyed on
`pwd -P` (`/private/tmp/...`) the bearing user message was 45357 chars and held
both markers (measured with `grep -c CODEWORD-TAIL-OMEGA` and a python `len()`
over the raw output; original AGENTS.md length 41071 chars). The CLI override
`-c project_doc_max_bytes=131072` gave the same full delivery with no trust entry.
A wrong trust path (`/tmp/...`) silently dropped the project config and the
default 32 KB cutoff truncated the tail marker.

Decision or Follow-up. The fixture is pinned for the Phase 2 and 3 instruction
gates (`codex.agents-md-size`, `instructions.zones`) so they cost no Astra quota
and stay deterministic. Two gotchas recorded in docs/runtimes.md: raise
`project_doc_max_bytes` above the 32 KB default to avoid silent AGENTS.md
truncation, and on macOS key project trust entries on the resolved `/private/tmp`
path, not `/tmp`.
## S0.4 Headless auth and environment isolation

Question. Does codex exec run headless from a fully stripped, non-login
environment? Does CODEX_API_KEY in the environment win over the ChatGPT tokens in
auth.json? What does --ignore-user-config actually suppress, and how does -p
interact with it? Do 3 concurrent runs against one CODEX_HOME race or corrupt
auth.json?

Method (all against codex-cli 0.153.4, isolated temp CODEX_HOME seeded from a copy
of the real auth.json; model gpt-5.4-mini; every live call ends with < /dev/null).

```
T=$(mktemp -d); mkdir -p "$T/home" "$T/proj"
cp ~/.codex/auth.json "$T/home/auth.json"; chmod 600 "$T/home/auth.json"

# (a) stripped env
env -i HOME="$HOME" PATH=/usr/bin:/bin:/opt/homebrew/bin:"$HOME/.local/bin" CODEX_HOME="$T/home" \
  codex exec --json --skip-git-repo-check -C "$T/proj" -m gpt-5.4-mini \
  "Reply with exactly the single word: HEADLESS_OK and nothing else." < /dev/null

# (b) env-var precedence
CODEX_API_KEY=sk-bogus CODEX_HOME="$T/home" \
  codex exec --json --skip-git-repo-check -C "$T/proj" -m gpt-5.4-mini "Reply with exactly: PRECEDENCE_OK" < /dev/null

# (c) --ignore-user-config and -p, with AGENTS.md codeword + SessionStart marker hook
printf 'When asked for the project codeword, answer with exactly: ZEBRAFISH42\n' > "$T/home/AGENTS.md"
# $T/home/hooks.json SessionStart runs: echo SESSIONSTART_MARKER > $T/home/marker.txt
printf 'model_reasoning_effort = "low"\n'  > "$T/home/config.toml"
printf 'model_reasoning_effort = "high"\n' > "$T/home/hi.config.toml"
CODEX_HOME="$T/home" codex exec ... --dangerously-bypass-hook-trust "What is the project codeword? ..."               # control
CODEX_HOME="$T/home" codex exec ... --ignore-user-config --dangerously-bypass-hook-trust "What is the project codeword? ..."  # test
CODEX_HOME="$T/home" codex exec ... -p hi "Reply with exactly: P_OK"
CODEX_HOME="$T/home" codex exec ... -p hi --ignore-user-config "Reply with exactly: P_OK"

# (d) concurrency
for i in 1 2 3; do ( CODEX_HOME="$T/home" codex exec ... -m gpt-5.4-mini "Reply with exactly: CONC_$i" < /dev/null; echo $? > "$T/d$i.rc" ) & done; wait
```

Result.
- Binary location drift from the brief. codex is at ~/.local/bin/codex (standalone,
  symlink into ~/.codex/packages/standalone/current/bin/codex); /opt/homebrew/bin/codex
  does not exist. A stripped PATH must include ~/.local/bin.
- (a) exit 0. Model returned HEADLESS_OK. Headless from env -i works with only HOME,
  PATH (containing the codex binary), and CODEX_HOME. The exec --json stream uses
  item.* events; the older event_msg shape is only in the rollout files.
- (b) exit 1. CODEX_API_KEY=sk-bogus flipped auth to ApiKey mode
  (auth.recovery_reason="not_chatgpt_auth") and the request got 401
  invalid_api_key ("Incorrect API key provided: sk-bogus"). The environment
  variable wins over the ChatGPT tokens in auth.json.
- (c) --ignore-user-config only skips $CODEX_HOME/config.toml. $CODEX_HOME/AGENTS.md
  (codeword ZEBRAFISH42) still reached the model and the SessionStart hook still
  wrote its marker, both with and without the flag. -p hi layered
  hi.config.toml on top of base config (reasoning_effort low -> high), but
  -p hi --ignore-user-config dropped both base and profile, falling back to the
  built-in default medium. So --ignore-user-config suppresses the profile layer too.
- (d) All 3 runs exit 0 with distinct output and 3 rollouts written. auth.json mtime
  and last_refresh are unchanged (token not near expiry, so no refresh fired; a
  refresh was deliberately not forced to avoid rotating the shared live token). A
  benign non-fatal race surfaced: concurrent runs collide installing the shared
  system-skills dir (ERROR codex_skills_extension::host_service: failed to install
  system skills ... remove existing system skills dir), runs still succeed.

Decision. Headless automation must (1) point PATH at ~/.local/bin (not
/opt/homebrew/bin) on standalone installs; (2) never leak CODEX_API_KEY into a
ChatGPT-auth environment, since it silently overrides and 401s; (3) treat
--ignore-user-config as config.toml-plus-profile-only, and still isolate AGENTS.md
and hooks.json by controlling CODEX_HOME; (4) give each concurrent worker its own
CODEX_HOME to avoid the skills-dir race. Follow-up: the concurrent-refresh path is
unproven; retest near token expiry in a throwaway home before relying on it.

## S0.8 Project layer trust gating

Question. Is a project-local .codex/config.toml read by codex exec only when the
project is trusted in the user config, and is it skipped otherwise?

Method. Temp project whose .codex/config.toml sets model = "bogus-model-xyz". Run
once with no [projects] entry in the temp user config and once with a trust entry.
Base user config sets model = "gpt-5.4-mini" as the fallback so the untrusted arm
never calls an unsanctioned model.

```
T=$(mktemp -d); mkdir -p "$T/home" "$T/proj/.codex"
cp ~/.codex/auth.json "$T/home/auth.json"; chmod 600 "$T/home/auth.json"
printf 'model = "bogus-model-xyz"\n' > "$T/proj/.codex/config.toml"
PROJ_REAL=$(cd "$T/proj" && pwd -P)

# untrusted
printf 'model = "gpt-5.4-mini"\n' > "$T/home/config.toml"
CODEX_HOME="$T/home" codex exec --json --skip-git-repo-check -C "$T/proj" "Reply with exactly: E_OK" < /dev/null

# trusted
{ printf 'model = "gpt-5.4-mini"\n\n'; printf '[projects."%s"]\ntrust_level = "trusted"\n' "$PROJ_REAL"; } > "$T/home/config.toml"
CODEX_HOME="$T/home" codex exec --json --skip-git-repo-check -C "$T/proj" "Reply with exactly: E_OK" < /dev/null
```

Result.
- Untrusted: exit 0, resolved model = gpt-5.4-mini. The project config.toml was not
  read.
- Trusted: exit 1, resolved model = bogus-model-xyz, API returned 400
  invalid_request_error "The 'bogus-model-xyz' model is not supported when using
  Codex with a ChatGPT account." The project config.toml was read and layered on top.
- The trust-table key must be the canonicalized path. macOS mktemp -d returns a
  /var/folders (or /tmp) path that is a symlink into /private/...; codex canonicalizes
  cwd before matching, so writing the unresolved path makes the trusted arm behave
  like the untrusted arm. Top-level keys in the user config must precede the first
  [projects."..."] table.

Decision. hex must write [projects."<canonical abs path>"] trust_level = "trusted"
(resolved with pwd -P / realpath) into the user config before it can rely on any
project .codex/config.toml being honored. Untrusted projects are safe by default:
their config layer is silently ignored.

## S0.9 Quota and token accounting

Question. Where do rate limits and token usage live in a headless run, and does a
turn that makes multiple tool calls count as one message for quota?

Method. One codex exec run (gpt-5.4-mini, read-only sandbox) that makes 3 shell tool
calls in a single turn, then extract the token_count records from the temp
CODEX_HOME rollout.

```
T=$(mktemp -d); mkdir -p "$T/home" "$T/proj"
cp ~/.codex/auth.json "$T/home/auth.json"; chmod 600 "$T/home/auth.json"
CODEX_HOME="$T/home" codex exec --json --skip-git-repo-check -C "$T/proj" -m gpt-5.4-mini -s read-only \
  "Run these three shell commands one at a time using your shell tool: first 'pwd', then 'date +%Y', then 'echo TOOLCALL_DONE'. After all three, reply with the single word FINISHED." < /dev/null
rf=$(find "$T/home/sessions" -name '*.jsonl' | head -1)
# count token_count records and read total_token_usage.total_tokens per record
```

Result.
- Rate limits live in the rollout as event_msg records of type token_count, each
  with info (total_token_usage, last_token_usage, model_context_window) and a
  rate_limits object (limit_id, primary.used_percent, primary.window_minutes,
  primary.resets_at, credits, plan_type). A scrubbed copy is committed at
  tests/fixtures/codex/rate-limits.json.
- The 3-tool-call turn produced 4 token_count records: one per model step (initial
  plus one after each tool-call result), each with its own rate_limits snapshot.
- info.total_token_usage.total_tokens accumulates across the invocation
  (11876 -> 23868 -> 35965 -> 48119); info.last_token_usage is the per-step usage.
- primary.used_percent was 0.0 for every record (10080-minute window, plan_type pro).
  Whether a multi-tool-call turn counts as one server-side message is NOT resolvable
  from a used_percent delta at this utilization; 0.0 -> 0.0 is a null, not a finding.

Decision. For quota tracking hex should read the rate_limits snapshot from the last
token_count record of a run (used_percent, window_minutes, resets_at), and treat
token usage as cumulative per invocation via total_token_usage, not per tool call.
Follow-up: re-measure the used_percent delta on an account with meaningful
utilization to settle the per-message-versus-per-turn question.
# Codex parity spikes (Phase 0)

Dated research ledger for the codex parity plan. One entry per spike. Each entry
gives the question, the exact commands, the result, and a decision or follow-up.
Plain language. No em dashes.

## S0.7 goose codex provider

Question. How does goose 1.46.0 drive the codex CLI? What provider and model
selection does it use, what approval and sandbox policy shows up in the codex
rollout, and does it honor CODEX_COMMAND and CODEX_REASONING_EFFORT?

Method (exact commands). All runs used a temp HOME and temp CODEX_HOME so no
real config was touched.
```
T=$(mktemp -d /tmp/gooseprobe.XXXXXX)
mkdir -p "$T/home" "$T/home2" "$T/codexhome" "$T/bin"
cp ~/.codex/auth.json "$T/codexhome/auth.json"; chmod 600 "$T/codexhome/auth.json"
printf 'model = "gpt-5.4-mini"\n' > "$T/codexhome/config.toml"
# wrapper logs argv + whitelisted env, then exec's the resolved real codex path
# recipe sets settings.goose_provider=codex, settings.goose_model=gpt-5.4-mini
goose run --recipe "$T/recipe.yaml" --render-recipe            # offline validation
# RUN A: auto + effort high
env HOME="$T/home" CODEX_HOME="$T/codexhome" CODEX_COMMAND="$T/bin/codex-wrap.sh" \
    CODEX_REASONING_EFFORT=high CODEX_SKIP_GIT_CHECK=true GOOSE_MODE=auto \
    GOOSE_DISABLE_KEYRING=true GOOSE_DISABLE_SESSION_NAMING=true \
    goose run --no-session -q --recipe "$T/recipe.yaml" < /dev/null
# RUN B: approve, effort unset, GOOSE_CODEX_DEBUG=1
# RUN C: fresh HOME ($T/home2), auto, CODEX_REASONING_EFFORT=low
# then read newest rollout turn_context from $T/codexhome/sessions/**/rollout-*.jsonl
```

Result.
- goose 1.46.0 has three codex providers. The one named `codex` shells out to the
  codex CLI and is deprecated in favor of `chatgpt_codex` (OAuth HTTP) and
  `codex-acp` (ACP adapter).
- Provider and model come from `--provider`/`--model`, or `GOOSE_PROVIDER`/
  `GOOSE_MODEL`, or a recipe `settings.goose_provider`/`settings.goose_model`.
  Non-interactive mode is `goose run --no-session -q`. There is no goose `--yolo`
  flag; `GOOSE_MODE` (auto, approve, smart_approve, chat) selects approval mode.
- goose invokes `<CODEX_COMMAND> exec -c model_reasoning_effort="<effort>" --json
  [--yolo] -` and pipes the prompt to codex stdin. The argv has no `-m`,
  `--model`, or `-c model=` token in any of the three runs, so goose forwards no
  model; codex took its model from the temp CODEX_HOME config.toml, which the
  probe pinned to gpt-5.4-mini (turn_context.model confirmed gpt-5.4-mini).
  `CODEX_SKIP_GIT_CHECK=true` was set but never reached the argv, so goose does
  not translate it to codex `--skip-git-repo-check`.
- GOOSE_MODE=auto passes `--yolo`. The codex rollout turn_context then shows
  `approval_policy = "never"` and `sandbox_policy = {"type": "danger-full-access"}`.
  So yes, auto maps to danger-full-access plus never. GOOSE_MODE=approve drops
  `--yolo` from the argv (observed); the resulting codex default policy for the
  approve run was inferred, not read from a turn_context.
- CODEX_COMMAND is honored: the wrapper ran on every invocation, and
  GOOSE_CODEX_DEBUG=1 printed `Command: "<wrapper path>"`.
- CODEX_REASONING_EFFORT is honored: `low` produced `model_reasoning_effort="low"`
  in the argv; when unset the effort defaulted to `high`.
- Captured argv committed at `tests/fixtures/codex/goose-codex-argv.txt`
  (temp paths redacted to <TMP>, env whitelist only, no secrets).

Decision or follow-up. For codex parity, goose already produces the yolo mapping
(auto to danger-full-access plus never). The env var for the `codex` CLI provider
is `CODEX_REASONING_EFFORT` (the `chatgpt_codex` provider uses the
`CHATGPT_CODEX_` prefix instead). Follow-up: the `codex` provider is deprecated
in 1.46.0, so a later phase should decide whether to standardize on `codex-acp`
or `chatgpt_codex`. Environment correction for all tasks: the codex 0.153.4 binary
is at `~/.local/bin/codex`, not `/opt/homebrew/bin/codex` as the brief states
(version and ChatGPT login match).
- The `trusted_hash` is a hash of a normalized identity, not of hook source
  text. Pipeline: build identity `{ event_name (snake_case key label), matcher,
  hooks: [one normalized handler] }`, then `toml::Value::try_from` (which drops
  every `None` field because TOML has no null), then `serde_json::to_value`,
  then `canonical_json` (recursively sort object keys), then
  `serde_json::to_vec` (compact), then SHA-256, formatted `sha256:<hex>`.
- The state key is `<abs hooks.json path>:<event_label>:<group_index>:<handler_index>`.
- Managed hooks (System `/etc/codex/requirements.toml`, MDM, enterprise) are not
  trust-hashed at all: `is_managed` gives `HookTrustStatus::Managed`, they are
  always enabled, and `hook_trusted_hash` returns `None` for them (no
  `[hooks.state.*]` entry). Trade-off recorded in docs/runtimes.md under
  "Managed hooks and requirements.toml".
- Normalization before hashing: `command_windows` forced to `None`; `timeout`
  defaulted (600 floor 1 for most events; 1 clamped to `[1,3]` for
  session_end/interrupt); `additional_context_limit` kept only for
  pre_tool_use/post_tool_use/session_start/user_prompt_submit/subagent_start and
  dropped when equal to the 2500 default; matcher forced to `None` for
  user_prompt_submit/stop/interrupt.
- `system/harness/src/codex_hook_hash.rs` implements
  `hook_hash(event_name, matcher, handlers_json) -> Result<String, String>` and
  is registered in `system/harness/src/lib.rs`. The unit tests derive two hashes
  by hand (SessionStart no matcher; PreToolUse with matcher) and pass:
  `sha256:3524dc80a43d23e5b183b4775038027cc6e152a7d9a8f8b0cd49c90a3410ccdf` and
  `sha256:a0fed18c4c7a2b85d069b4b7afb578daa0c412c668f819ee9e14b894a11156cb`.
- Dependency divergence: codex-config resolves `toml` 0.9.11; this harness pins
  `toml` 0.8. Output is identical for the `Value` variants a hook identity uses.

Decision or Follow-up.

- Stay on `toml` 0.8 in the harness (spec says use existing deps; 0.8 and 0.9
  serialize these `Value` variants identically). The divergence risk is retired
  by the live test.
- PENDING run: `trusted_hash_matches_codex_written_entry` is `#[ignore]`d and
  needs Mike to trust at least one JSON hook on this machine, which writes a
  `[hooks.state."<key>"] trusted_hash` entry into `~/.codex/config.toml`. After
  that, run
  `cargo test --manifest-path system/harness/Cargo.toml codex_hook_hash -- --ignored`
  to confirm the reproduction against a hash Codex itself wrote.

## S0.10 Claude MCP add-json user scope

Question: what exact JSON does `claude mcp add-json -s user` write for an http
server with headers and for a stdio server, and how does remove behave in the
user scope.

Method (exact commands, HOME set to a throwaway temp dir for every call so the
real ~/.claude.json stays untouched):

    T=$(mktemp -d); mkdir -p "$T/home"
    HOME="$T/home" claude mcp add-json -s user probe-http '{"type":"http","url":"https://example.invalid/mcp","headers":{"X-Test":"1"}}'
    HOME="$T/home" claude mcp add-json -s user probe-stdio '{"type":"stdio","command":"npx","args":["-y","some-mcp-server"],"env":{"API_KEY":"xxx"}}'
    HOME="$T/home" claude mcp list
    HOME="$T/home" claude mcp remove -s user probe-http

Result:

- Claude Code version 2.1.263.
- Both add-json calls exit 0. Messages: `Added http MCP server probe-http to
  user config` and `Added stdio MCP server probe-stdio to user config`.
- User scope writes to the top-level `mcpServers` key of `<HOME>/.claude.json`.
  The payload is stored verbatim under the server name. Running from a project
  subdirectory does not change placement and creates no `projects` entry.
- The written file is mode 0600. The first run also creates
  `<HOME>/.claude/backups/`.
- `claude mcp remove -s user probe-http` exits 0, prints `Removed MCP server
  probe-http from user config` and `File modified: <TMP>/home/.claude.json`,
  and leaves the other server in place.
- A second remove of the same name exits 1 with `No MCP server named
  "probe-http" in user scope`.
- A scrubbed copy of the written file is committed as
  tests/fixtures/claude/mcp-add-json-user.json (machineID and userID redacted).

Decision or Follow-up: for Codex parity, target the flat top-level `mcpServers`
map in one JSON file, keyed by server name, with the value equal to the add-json
payload. Treat remove as scope scoped and expect a nonzero exit when the name is
absent.

## S0.6 Effort levels

These three facts are pinned from prior verification on 2026-09-07. They were
not re-run in this task, because the spec forbids using gpt-6-astra in spikes
(shared quota with Mike's interactive sessions). Recorded here for the record.

- gpt-6-astra accepts `-c model_reasoning_effort=ultra` (exit 0, 2026-09-07).
- The model catalog lists low, medium, high, xhigh, max, ultra for gpt-6-astra,
  with default low (2026-09-07).
- The API key on this Mac lists gpt-6-astra in /v1/models (2026-09-07).
