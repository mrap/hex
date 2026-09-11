# macOS app install transaction

`system/scripts/macos-app-install.py` owns filesystem publication for the Hex,
BOI, `scipd`, and `cq` app bundles. It does not sign, verify signatures, or
invoke services.
Callers inject the accepted signer boundary.

## Public API

The caller supplies a fixed product name, its product root, a build artifact,
and a signer adapter:

```text
install(product, root, source, signer, policy_path=central_policy_path())
detect_mode(product, root, policy_path, signer)
```

The signer adapter exposes `stage(source, product, policy, candidate, receipt)`
and `verify_installed(bundle, product, policy, expected_state)`. It owns all
code-signing and Apple metadata checks. The transaction component
compares the public result with state but does not reproduce cryptographic
checks.

For service consumers, use a read-only `service_owner` wrapper that holds the
product lock before calling `verify_installed`. The wrapper returns the fixed
bundle path, executable path, compatibility path, product ID, generation,
public signature expectations, current hashes, and helper provenance. A
caller that already holds the lock must pass the inherited lock context rather
than acquire a nested lock.

## scipd service reconciliation

`service-reconcile code-intel-daemon --root ~/.codeintel` is the narrow launchd
operation for the existing `com.hex.scipd` user service. It verifies the signed
owner and helpers while holding the code-intel product lock, then reads the
existing `~/Library/LaunchAgents/com.hex.scipd.plist` without following a
symlink. It changes only `ProgramArguments` and the singleton
`AssociatedBundleIdentifiers` value. It preserves other plist settings.

The operation never creates an absent plist or starts an unloaded service. It
may update an existing stopped plist without starting it. It restarts only a
service that was loaded before the transaction. A restart failure after plist
publication returns nonzero with `published: true`; callers must inspect that
result before retrying. `code-intel-cli` is not a service and is rejected by
this command. `--dry-run` performs no plist or launchd mutation and reports
`service_needs_change`.

## Fixed locations

The machine policy is shared:

`~/Library/Application Support/Hex/build-signing/policy.json`

Product state is separate:

- BOI: `~/.boi/BOI.app.install-state.json`
- Hex: `$HEX_DIR/.hex/Hex.app.install-state.json`
- scipd: `~/.codeintel/SCIPD.app.install-state.json`
- cq: `~/.codeintel/CQ.app.install-state.json`

The code-intel products share the `~/.codeintel` root but use independent
compatibility paths and helper directories:

- scipd: `bin/scipd`, `libexec/scipd`, bundle ID `com.mrap.hex.scipd`
- cq: `bin/cq`, `libexec/cq`, bundle ID `com.mrap.hex.cq`

Per-product transaction names are fixed under the product root:

- Lock: `.<product>.app-install.lock`
- Journal: `.<product>.app-install.journal.json`
- Rollback: `.<product>.app-install-rollback-<transaction_id>/`

The BOI standalone verifier is installed at `~/.boi/libexec/macos-signing.py`
as exact accepted Foundation helper bytes. Its public SHA-256 is recorded in
state. It resolves the same central policy by default. The installer copies
the helper, never the policy.

## State and modes

State schema version `1` records product, mode, fixed bundle ID, canonical app
and executable paths, compatibility path, Team ID, certificate fingerprint,
designated requirements, Mach-O UUIDs, app and executable hashes, generation,
source revision (the caller's Git SHA), signer helper hash, a `helpers` map containing both
`macos-signing.py` and `macos-app-install.py` hashes and source
revisions (their paths are fixed under the product's `libexec` directory),
previous compatibility identity, and transaction ID. It contains no secrets.

The supported modes are `empty`, `legacy-raw`, `configured-legacy`,
`signed-current`, `signed-policy-missing`, and `ambiguous`. A known signed
state with a missing central policy blocks replacement, downgrade, and service
ownership checks. The component never replays recorded signature metadata as
current verification. An unconfigured legacy install
remains unchanged until explicit signed migration.

## Transaction rules

The component acquires the lock with a nonblocking `flock`. Busy is an
immediate error. It holds the lock across staging, app publication,
compatibility-path publication, state commit, and recovery. It opens directory
descriptors for both the app parent and the `bin` compatibility parent and
revalidates device and inode identity before each swap.

Candidates stay beside the final app. A missing app uses macOS
`renameatx_np(..., RENAME_EXCL)`. An existing app uses
`renameatx_np(..., RENAME_SWAP)` with the candidate in the same parent. The
old app moves to a unique rollback directory with no-clobber publication. The
component never deletes or renames over an unknown app.

The legacy raw CLI path is migrated by swapping a prepared symlink or launcher
with the old regular file. The old raw file moves to the transaction rollback
directory. The path is never unlinked first. The state record commits only
after app and CLI paths point to the same candidate generation.

The bounded journal records schema, product, transaction ID, fixed-root paths,
old app and CLI identities, candidate hash, phase, and rollback names. An open
journal blocks a new transaction. Recovery may finish or roll back only when
the current app and CLI identities still match the journal candidate. If an
actor replaced either path, recovery stops loudly and preserves that actor's
replacement.

## Verification limits

Tests inject a fake signer for orchestration and use actual macOS filesystem
syscalls for no-clobber publication, directory swap, and raw-to-symlink
migration. They must assert that no `codesign`, `security`, `launchctl`, or
`systemctl` command runs. A skipped non-macOS syscall test is not an
atomicity qualification.

## CLI boundary

Commands use `COMMAND PRODUCT --root ROOT`:

```text
python3 macos-app-install.py mode boi --root ROOT
python3 macos-app-install.py preflight boi --root ROOT --lock-fd FD
python3 macos-app-install.py verify-current boi --root ROOT --lock-fd FD
python3 macos-app-install.py service-owner boi --root ROOT --lock-fd FD
python3 macos-app-install.py install boi --root ROOT --source SOURCE --version VERSION --source-revision PRODUCT_GIT_SHA --helper-source-revision FOUNDATION_GIT_SHA
```

The signer is always the regular `macos-signing.py` sibling beside this
script. Callers cannot select an arbitrary signing helper.

`service-owner` returns `mode: signed-current` after it verifies the installed
bundle, state, helper hashes, and current paths. It fails when the central
policy is absent. Service-changing callers must hold the product lock.

Read-only service commands return JSON with `schema_version`, `product`,
`mode`, `executable_path`, `bundle_identifier`, and `generation`, plus public
hashes and helper provenance. `--lock-fd` is inherited from the caller's
already-held product lock. The helper never acquires a nested lock.


## Final publication checks

The CLI accepts only the central machine policy path. An explicit `--policy`
may name that same path, but cannot select a product-specific policy. Unit-level
API fixtures may supply a disposable policy path. Policy validation always
executes the exact bounded, retained source bytes of the regular signer sibling.
A missing policy reader is an error, not permission to use a weaker validator.

Each copied helper is bounded to one MiB and checked against its recorded hash
before publication. The candidate verifier must report exactly the requested
version before any public path changes. The state records that verified version.
Hex publishes both `bin/hex` and `bin/hex-agent` in the same guarded transaction.

The child adapter clears inherited environment variables except HOME, a fixed
system PATH and locale. It drains stdout and stderr concurrently through pipes,
with a separate 64 KiB limit for each stream and one total deadline. No output
spools to disk. The adapter requires the validated product lock.

## Signing child ownership

A transient guardian starts in a separate process group. It receives the
validated lock descriptor and only the read end of a private liveness pipe.
Only the installer owns the writer. READY precedes GO, so signing cannot start
before ownership is established. Installer exit, including an outer caller's
SIGKILL, closes liveness without killing the guardian.

The guardian starts a separate signer work group. Its bootstrap executes the
existing signer CLI, flushes its output, sends a bounded completion frame and
remains alive as the unreaped group anchor. The guardian signals that owned
group before reaping the leader. It drains pipes and verifies group absence
before releasing any result. Normal completion can clean up a pipe-holding
child and then succeed. A missing completion frame remains an error.

The parent requests cancellation by closing liveness and waits through a bounded
cleanup interval. It never kills a guardian that may still own work. An
exceptional surviving guardian receives one blocking reaper wait while the
parent remains alive; the OS adopts it if the parent exits. This is not a
persistent service or recurring polling job.

Rust consumers provide both previously verified helper byte strings through
the existing `contents` binding. Standalone source CLI entry pins both siblings
once. Internal launches forward those exact bytes through bounded inherited
pipes. They do not reread a path and bless a new hash. Incomplete or changed
frames cannot execute. Both existing helper provenance records cover this code;
there is no third helper or environment-selected implementation.

Darwin can return EPERM when signaling an unreaped zombie-only group. A failed
signal does not mean cleanup succeeded. Bounded EOF, reap and subsequent
read-only group absence are still required. A living owner or remaining group
keeps cleanup incomplete. No destructive signal occurs after reap.

## Incomplete cleanup

Incomplete cleanup retains the live guardian and its product lock. Lock
references are closed, never explicitly unlocked through the shared inherited
file description. Success or automatic unlock is not a cleanup-error fallback.

The diagnostic receipt is `.<product>.app-cleanup-failure.json` under the product
root. It contains bounded public failure details and a live control endpoint.
Recorded process IDs are diagnostic only. Recovery never signals a stored PID.

```text
python3 macos-app-install.py cleanup-status boi --root ROOT
python3 macos-app-install.py cleanup-retry boi --root ROOT
```

Before GO, the guardian reserves a private socket under the existing canonical
`/private/tmp` directory. Its deterministic name includes the user ID and a hash
of the product/root pair. Existing endpoints are preserved and block new work.
Normal operations remove this transient socket and create no diagnostic receipt
or journal. The socket contains no credential or policy data.

If diagnostic storage fails, `cleanup-status` still locates the live guardian
without a receipt and reports the storage error. The guardian waits for explicit
status/retry requests, not a recurring scan. A retry uses its still-owned anchor
or read-only post-reap observation. It releases its lock reference and endpoint
only after cleanup is proven. Unresolved cleanup stays blocked. If the guardian
itself is killed or the kernel cannot respond, the command reports unavailable
recovery; it does not act on recorded process IDs.

These guarantees cover the deliberately nested signer and cooperative children
that stay in its work group. They do not claim containment of hostile detached
processes, a killed guardian, or an unresponsive kernel. Ordinary verification
changes no bundle, central policy, signed state, journal or diagnostic receipt.

Before the committed journal marker, the transaction syncs the app parent,
compatibility parent, helper parent and rollback directory. Journal removal also
syncs the app parent. Failures remain loud, retain the journal and use the existing
identity-checked rollback. An actor replacement stops rollback without deleting
that actor's data. A partial publication is reported as such; it is not silently
removed or described as a clean install.


## Abandon a qualified staging failure

`python3 -I -B macos-app-install.py abandon-staging PRODUCT --root ROOT`
archives one failed prepublication attempt under
`ROOT/.PRODUCT.abandoned-staging-TRANSACTION`. It does not sign, restore an old
installation, publish a new installation, or change a service. A successful
result has `action: abandoned-staging`, `published: false`, and `archive_path`.
The next installation is a fresh transaction.

New journals record the exact public app, CLI, alias, state and helper prestate,
plus bound parent identities. App content is included. A caught staging failure
also records candidate, receipt and rollback identities. Abandonment requires
those records, an unchanged public prestate, unchanged staging evidence, and an
absent or empty rollback directory. The product lock remains held throughout.
The command prepares and syncs an exclusive archive manifest, moves only the
matching evidence without replacing existing entries, then moves the journal
last. All evidence remains available in that archive. It never deletes an actor
replacement or rewrites any public product file.

Old journals, later publication phases, and hard crashes without retained owned
staging evidence remain blocked for manual review. A candidate-shaped filename
alone is not sufficient evidence. This is cooperative local recovery, not a
hostile same-user filesystem sandbox or complete post-crash rollback protocol.
If archiving fails partway, the error reports `published: null` and the archive
path. Keep both locations intact. The journal remains a blocker until its final
move; do not delete it to force a retry. A final sync error after that move may
leave a complete archive with no journal, and still reports incomplete recovery.
No automatic retry of a partial archive is supported.
