# macOS install callers

On macOS, `install.sh` delegates signed BOI and Hex publication to the common
`system/scripts/macos-app-install.py` transaction. The shell installer does not
publish an app bundle, write app state, or decide whether an install is legacy
or signed.

The caller invokes these commands with `/usr/bin/python3 -I -B`:

```text
mode PRODUCT --root ROOT
preflight PRODUCT --root ROOT
verify-current PRODUCT --root ROOT
install PRODUCT --root ROOT --source PATH --version VERSION --source-revision REVISION --helper-source-revision FOUNDATION_REVISION
service-reconcile PRODUCT --root ROOT
compatibility-alias PRODUCT --root ROOT --hex-workspace WORKSPACE
```

Configured and previously signed products stay on the common transaction path.
The caller verifies the current app before a same-version BOI fast path and
compares it with the pinned BOI tag revision. A managed source build keeps its
managed status through the build and fails if signed evidence disappears.
Signer or transaction failure does not fall back to a raw destination copy.
Managed Hex prebuilt downloads are unsupported until the release bytes have
verifiable source provenance. Legacy unconfigured macOS installs may still use
the raw prebuilt path. The product revision and Foundation helper revision are
recorded separately.

Non-macOS behavior stays unchanged. A truly unconfigured legacy macOS install
keeps its existing raw path until the common mode command reports a managed
state.

The source installer prepares both code-intel products before a build and
rechecks them after the build. A signed-current product is reusable only when
its recorded source revision equals the selected checkout revision. It reads the version from
`system/code-intel/Cargo.toml`, publishes `cq` and `scipd` through the common
transaction at `~/.codeintel`, and never copies them into `.hex/bin` when the
products are managed. It records the source checkout revision before the build
and refuses publication if the checkout becomes dirty or moves. Each Cargo
build uses a fresh private target directory, passes the local `rustc` host
target explicitly, and requires both package binaries at that target's release
path. An old artifact cannot satisfy the exact output check.

After both managed products publish, the caller invokes
`service-reconcile code-intel-daemon --root "$HOME/.codeintel"`. It accepts only
schema 1, the exact product, `signed-current`, fixed owner paths, and the actions
`loaded`, `stopped`, `absent`, `updated-stopped`, `restarted`, or `recovered`.
Changed actions must report `service_needs_change=true` and `published=true`.
Every successful response also carries `service_recovery_pending`; a healthy
stopped or unloaded dry-run is accepted without requiring a restart. When a stale
daemon has a validated pending marker, the caller settles that marker before
publishing a replacement app.
The common operation keeps absent or stopped services stopped. Any reconciliation
error fails the install.

The caller also invokes the shared `compatibility-alias` operation for each
managed product. It guards `.hex/bin/cq` and `.hex/bin/scipd` against stale raw
executables shadowing the signed products. A correct alias is a no-op. A fixed
raw entry is migrated through the common guarded operation. Foreign links and
unexpected file types fail without overwrite. A current signed product and its
alias do not trigger a rebuild or republish.
