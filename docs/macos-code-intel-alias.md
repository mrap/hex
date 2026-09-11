# Migrate old Hex code-intel commands

The shared installer provides one compatibility operation for native upgrade
and shell installation. It does not build, sign, load or restart a service.

```text
macos-app-install.py compatibility-alias code-intel-cli --root HOME/.codeintel --hex-workspace HEX_WORKSPACE [--dry-run]
macos-app-install.py compatibility-alias code-intel-daemon --root HOME/.codeintel --hex-workspace HEX_WORKSPACE [--dry-run]
```

The root must be the canonical current user's `HOME/.codeintel`. The Hex
workspace and its existing `.hex/bin` parent must be real, user-owned
directories. There is no alias filename or target override.

| Product | Hex compatibility path | Exact absolute link target |
|---|---|---|
| `code-intel-cli` | `HEX_WORKSPACE/.hex/bin/cq` | `HOME/.codeintel/bin/cq` |
| `code-intel-daemon` | `HEX_WORKSPACE/.hex/bin/scipd` | `HOME/.codeintel/bin/scipd` |

The operation holds the existing product lock while the shared service-owner
verifier validates the signed app, current policy, helper provenance and product
state. Verification does not search for a private signing key. A missing policy
or invalid installed app stops the operation even if the alias already matches.

An exact canonical alias is a no-op. A missing entry uses no-clobber atomic
publication. A regular old executable is archived before replacement. Foreign
or dangling aliases, special file types and concurrent entry changes are
preserved and fail loudly.

## Archive and publication

Each raw migration creates an exclusive private archive below
`HEX_WORKSPACE/.hex/.code-intel-compat-backups/<product>-<random>/`.
`previous-cq` or `previous-scipd` contains an independent copy of the old bytes.
The archive file is private; its original mode is recorded in `receipt.json`.
The receipt records the exact old identity/hash, fixed alias/target and verified
product source revision/generation. Its status is `prepared`, not a claim that
publication occurred.

The copy is streamed up to the inspected size, hashed, read back and synced.
The receipt and archive parents are synced before the public path changes.
Bound parent and old-entry identities are checked again before the single atomic
rename. The destination parent is synced afterward. The old executable is never
run or chmodded, and the archive is not a hard link to its mutable inode.

Failure before publication leaves the public old entry unchanged. Failure after
publication reports `published: true`, with the old bytes still archived. No
failure deletes that archive or claims the old path was never changed. Prepared
but unused archives remain available. A retry with a verified correct alias is
a no-op, so no separate recovery journal is needed for this single-path change.

This is the existing cooperative ownership boundary, not a filesystem-wide
compare-and-swap against arbitrary same-account writers.

## Results and callers

Success returns JSON fields `schema_version`, `product`, `source_revision`,
`generation`, `alias_path`, `target_path`, `action`, `changed`, `published` and
nullable `archive_path`. Actions are `current`, `would-create`, `would-migrate`,
`created` and `migrated`. Errors exit nonzero and preserve available partial
result fields on stderr.

Dry-run verifies and classifies only. It returns `changed: false` and
`published: false`; its action identifies pending work. It creates no alias,
archive, journal or signed-state change. Reading an old executable can update
filesystem access time. The signing verifier's transient cleanup socket has its
separate documented lifecycle.

Both callers must include alias-only drift in no-op and retry decisions. After
a product is verified current, run this operation before claiming its deployment
complete. If app publication succeeded but alias migration failed, report partial
completion and do not record overall upgrade success. Service reconciliation is
a separate shared operation and never occurs here.
