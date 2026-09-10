# Managed build targets

Foundation callers use `system/scripts/managed-target-check.py` before a later
caller-specific Cargo action. The check does not run Cargo, create target
directories, start BOI, or change configuration.

## Command

```sh
python3 system/scripts/managed-target-check.py \
  --caller foundation-install \
  --executable /absolute/path/to/caller \
  --source-revision COMMIT \
  [--target /absolute/path/to/target]
```

The receipt uses `boi.managed-target-check.v1`. It contains the accepted target,
selection source, policy revision, caller-supplied source revision, and the
canonical path and SHA-256 of the caller executable. Source revision is caller
context. It is not an attestation claim.

## Local Cargo gate

Use `system/scripts/managed-cargo-gate.py` for supported local Cargo work. It
accepts only `build`, `test`, and `clippy`, invokes the adjacent target adapter,
creates and rechecks an accepted missing target leaf, binds both Cargo output
variables, and retains a private attempt receipt. It does not run shell text or
an arbitrary command.

The supported grammar includes `--locked`, `--offline`, manifest and package
selection, and `--target-dir`. It accepts only `--config build.build-dir=PATH`
for a build-dir override, which must resolve to the checked root. It rejects raw
Cargo pass-through. `--deny-warnings` is valid only with `clippy` and maps to
the fixed Cargo suffix `-- -D warnings`.

The caller provides an existing private receipt directory. The gate requires an
absolute, non-symlinked directory owned by the current user and not writable by
group or others. Each invocation creates a unique mode-0600 receipt without a
full argv, environment, or credential values. A receipt failure before Cargo
prevents launch. A receipt update failure after Cargo reports both failures.
Every Cargo outcome prints only its receipt path, operation, and exit status.

The gate does not sandbox Cargo build scripts, govern raw terminal Cargo, or
replace hosted CI. Format guidance is outside this output-root contract.

## Selection and validation

The selected target follows this order:

1. `--target`
2. `CARGO_TARGET_DIR`
3. `BOI_CARGO_TARGET_DIR`
4. `~/.boi/v2/daemon.toml` `cargo_target_dir`

An empty value at any selected level fails. The check requires a nonempty policy
revision plus nonempty absolute `allowed_roots` and `denied_roots`. It resolves
existing aliases and missing leaves through their nearest existing parent. It
resolves each path component before applying a later `..`, so an alias cannot
escape a denied root. Denied roots take precedence over allowed roots.

V1 has one approved Cargo output root. A caller must bind both
`CARGO_TARGET_DIR` and `CARGO_BUILD_BUILD_DIR` to `resolved_target` before Cargo
starts. Never merely unset `CARGO_BUILD_BUILD_DIR`, because Cargo config can
select another location. A supported `build.build-dir` config override must be
rejected or pass `validate_same_root_build_dir()` before Cargo starts. The check
does not accept a second root or a `--build-dir` argument. Do not use shell
evaluation to build a Cargo command.

For a missing target leaf, a caller must pre-check it, create only the accepted
path, re-check that existing path and the policy revision, and only then launch
Cargo. If the recheck changes target or policy, the gate does not guess whether
the new leaf is safe to remove. It fails loudly with the retained created path.
This read-only CLI never creates the directory.

If `~/.boi/bin/boi` exists, the boundary invokes only `boi target check` and
strictly validates its receipt. A non-executable file, dangling link, failed
checker, malformed receipt, oversized receipt, timeout, or mismatched receipt
fails. It never falls back to bootstrap in those cases.

The adapter invokes the one BOI path with an argument array. It does not use a
shell or execute caller-supplied command text.

The adapter gives that checker its own process group. Timeout, oversized output,
and read failures kill the owned group before the checker leader is reaped.

The installed-BOI path does not parse `daemon.toml`. BOI owns that parsing, so
the ordinary adapter remains usable on Python 3.9.

If that exact BOI path is absent, the bootstrap validator reads `daemon.toml`
and applies the equivalent policy. Bootstrap requires Python 3.11 `tomllib`.
Python 3.9 reports `TOML_PARSER_UNAVAILABLE`; it does not install or vendor a
parser. This source slice does not change the Foundation Python support floor.

## Installed checker authority

The accepted installed BOI checker is the policy authority. Consumer receipt
validation checks the protocol and selected inputs. It does not authenticate
the checker or independently recompute its policy. In daemon-config selection,
the consumer trusts the accepted producer for the target and policy revision.

Before adoption, the rollout owner must verify that the compatibility path
resolves to the canonical signed BOI executable. Verify the installed registry,
installation state, source identity, executable hash, and signature through the
existing read-only managed installation verifier under its documented lock
rules. Use `system/scripts/macos-app-install.py` and its signing dependency.
Keep that provenance result separate from the eight-field target receipt.
Do not add another identity store or a per-command signing step.

`executable_identity` describes the caller-specified executable, not BOI.
`source_revision` is supplied context, not independent source attestation.
Fake-checker tests exercise the protocol, not installed provenance. The
operational V1 contract does not protect against hostile replacement by the
same account or lock every future pathname. It does not govern arbitrary shell
commands. Shared producer/bootstrap conformance remains an adoption requirement.

## Acceptance status

The boundary has synthetic protocol coverage. Installed-checker authority and
caller activation remain separate rollout checks. Do not treat local synthetic
coverage as a substitute for installed-authority verification.

The transient two-root receipt is superseded. A receipt with fields beyond the
eight-field V1 schema is rejected. Distinct output roots need a separate
coordinated contract.
