# Hex Versioning

## Source of Truth

`/version.txt` at the workspace root is the single source of truth for the hex version. Everything derives from this file:

- **Rust binary** — `build.rs` reads `version.txt` at compile time, injects it as `HEX_VERSION`. The binary prints `hex 0.8.0 (abc1234)` where the SHA comes from git.
- **Cargo.toml** — must match `version.txt`. This is a Cargo requirement, not the source of truth. The comment in Cargo.toml says this explicitly.
- **Foundation releases** — git tags (`v0.8.0`) must match `version.txt`. The release pipeline verifies this.
- **`hex version`** — reads the compiled-in `HEX_VERSION` + `HEX_GIT_SHA`.

## Version Flow

```
version.txt (source of truth)
    ├── build.rs reads → HEX_VERSION env at compile time → binary
    ├── Cargo.toml matches (manual sync, enforced by CI)
    └── git tag matches (enforced by release pipeline)
```

## Releasing a New Version

1. Update `version.txt` with the new version (e.g., `0.9.0`)
2. Update `Cargo.toml` version to match
3. Build: `cd .hex/harness && cargo build --release`
4. Install: `cp .hex/harness/target/release/hex .hex/bin/hex`
5. Verify: `hex version` shows new version + current SHA
6. Sync to hex-foundation
7. Tag: `git tag v0.9.0`
8. Push: `git push origin main --tags`

## Why version.txt

- The version belongs to the whole hex system (binary + scripts + skills + manifests), not just the Rust crate
- Non-Rust components (Python scripts, shell tools, YAML manifests) can read `version.txt` without parsing Cargo.toml
- The release pipeline can bump the version with a single `echo "0.9.0" > version.txt` without touching Rust files
- Foundation installs check `version.txt` to determine if an upgrade is available
