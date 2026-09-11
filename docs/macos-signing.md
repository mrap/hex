# macOS candidate signing

`system/scripts/macos-signing.py` stages a verified candidate app bundle. It does not install it, register an app, change a service, create an identity, or request protected-folder access.

## Policy

Managed installation uses the canonical machine policy described in the
[macOS build standard](macos-build-standard.md). The explicit policy argument
below is for standalone candidate staging and read-only verification, not a
second per-repository installation policy.

Use a local JSON policy containing only public identity configuration:

```json
{
  "schema_version": 1,
  "certificate_sha1": "0123456789ABCDEF0123456789ABCDEF01234567",
  "team_id": "TEAM123456",
  "keychain": "/Users/test/Library/Keychains/signing.keychain-db"
}
```

The optional keychain path must be absolute. It is passed to both identity lookup and signing. The fingerprint selects the exact certificate, not a name substring. The selected identity must already be available locally. The helper never falls back to ad-hoc signing. Do not put passwords, tokens, or private keys in the policy.

## Stage a candidate

```text
python3 system/scripts/macos-signing.py SOURCE hex POLICY.json OUTPUT.app --version 1.2.3 --receipt RECEIPT.json
```

The signer and installer use the same fixed product names:

| Product | Executable | Bundle identifier |
|---|---|---|
| `hex` | `hex` | `com.mrap.hex` |
| `boi` | `boi` | `com.mrap.boi` |
| `code-intel-daemon` | `scipd` | `com.mrap.hex.scipd` |
| `code-intel-cli` | `cq` | `com.mrap.hex.cq` |

Each product has its own app name and usage descriptions. Product support in
these helpers does not establish integration with a build or service caller.

The version must have three numeric components. The major component permits
0 through 9999; minor and patch permit 0 through 99. Zero-major Cargo versions
are supported without changing the release number.

Apple defines the numeric formats in
[`CFBundleVersion`](https://developer.apple.com/documentation/bundleresources/information-property-list/cfbundleversion)
and [`CFBundleShortVersionString`](https://developer.apple.com/documentation/bundleresources/information-property-list/cfbundleshortversionstring).
This helper accepts a bounded subset and rejects leading zeroes and prerelease
or build suffixes. It does not map arbitrary semantic versions into Apple build
versions.

The source must be regular and have no setuid, setgid, or sticky bits. The helper copies a pinned source inode into private staging and sets the staged executable to `0755`. It records the source mode and hash and rejects detected source changes before publication. It never signs or changes the source file.

Every reported Mach-O architecture must have a nonzero, well-formed UUID. The helper checks source signature flags and parses entitlements for each architecture. Unsigned code and code with only the ad-hoc/linker-signed flags are supported. Nonempty entitlements, unknown flags, missing metadata, or inspection errors fail closed. Other security-metadata preservation policies are outside this staging contract. No hardened-runtime, sandbox, entitlement, or custom designated-requirement flags are guessed.

Signing uses Apple's default timestamp server and fails if timestamping fails. Verification uses the actual Apple verifier with strict checking and an explicit Apple generic anchor, expected Team OU, and product identifier. It separately reads each architecture's designated requirement and public leaf certificate. It hashes the actual extracted `prefix0` DER file and compares that hash with the selected fingerprint. UUIDs must remain unchanged across signing.

The receipt records the source and signed executable hashes, modes, version, fixed identifier, Team ID, selected certificate fingerprint, and every architecture's UUID and designated requirement. Displayed authority names are not treated as trust evidence. The receipt contains no environment dump or key material.

## Publication and partial failures

The output and optional receipt must not exist, even as dangling symlinks. Parent aliases are canonicalized before overlap checks. Receipt paths inside the candidate or over protected inputs are rejected. The helper prepares all receipt bytes and hashes while the bundle is private, then publishes with macOS no-clobber rename using opened destination parent directories. Parent changes detected before publication are errors.

Bundle and receipt publication are separate operations, not a two-path transaction. A failure before bundle publication removes only private staging. A receipt publication failure after bundle publication exits nonzero and reports `published: true` with the computed receipt. The candidate stays in place. A closed stdout reader after successful staging also cannot trigger candidate deletion. Never infer that an error means no candidate exists; inspect the reported publication state before retrying.

The helper never deletes or overwrites the public output on failure. Another actor may have replaced that path. It does not claim atomic pathname stability against hostile concurrent parent-directory changes. The installer must use an owned staging parent and independently verify the candidate before an eventual switch. Private staging cleanup errors are also failures, with the truthful publication state retained.

## Verify an installed bundle without a signing key

```text
python3 system/scripts/macos-signing.py verify-installed BUNDLE PRODUCT POLICY.json
```

The Python API is `verify_installed(bundle, product_name, policy_path, run=run_command, timeout=COMMAND_TIMEOUT)`. This read-only path never calls identity search, signing, registration, or keychain import. It can verify with an unavailable signing key and a nonexistent optional keychain path. It uses the policy's public certificate fingerprint and Team ID directly with the shared Apple verifier.

Verification checks the actual fixed product identifier, app type/name, numeric version, product-specific usage text, and real executable inside the bundle. Nested symlink or nonregular components, missing/nonexecutable files, special mode bits, malformed or oversized Info.plist, and inconsistent metadata are errors. A bundle-root alias is resolved to its canonical path. The verifier checks every reported architecture, its UUID, designated requirement and extracted public leaf fingerprint, then rejects detected changes to the relevant components or their hashes during the check.

Certificate extraction uses a fresh private temporary directory under the existing canonical `/private/tmp`, outside the bundle. Verification ignores `TMPDIR` and never calls writable temporary-directory discovery. It fails if that fixed parent is unavailable or resolves inside the bundle. Verification writes no receipt file and changes no bundle content. Successful stdout JSON contains `verified: true`, canonical bundle/executable paths, relevant hashes, version and public signature metadata. Callers can capture stdout to their own external receipt. Errors return nonzero; a closed stdout reader does not trigger deletion or rollback.

The original staging CLI remains available unchanged. An existing staging source literally named `verify-installed` still uses the old form with its required `--version` argument.

This is an observation of current code, not a lock on future path contents or a live-process attestation. Installer/service callers must bind their eventual executable mapping to the verified candidate and recheck after a switch. Verification does not re-sign the app or impose a new entitlement/flag policy on already installed code. Certificate validity and identity are not the same as an OS privacy grant or notarization decision.

## Qualification limits

Tests use a strict command-boundary mock and real local filesystem publication. They verify arguments, failure routing, parsed plist values, source preservation, alias/collision behavior, and partial-result reporting. They do not prove certificate trust. The actual publication tests run only on macOS; pure validation tests are portable.

Certificate-backed signing, notarization policy, macOS privacy attribution, installed LaunchAgent association, live replacement, and unchanged permissions across a different rebuild remain separate acceptance checks. An installer must compile and test actual callers before using this helper.

Apple references: [code-signing requirements](https://developer.apple.com/library/archive/documentation/Security/Conceptual/CodeSigningGuide/RequirementLang/RequirementLang.html), [code-signing details](https://developer.apple.com/library/archive/technotes/tn2206/), and the installed `codesign(1)` and `security(1)` manuals. Apple defines ad-hoc and linker-signed flags in [XNU cs_blobs.h](https://github.com/apple-oss-distributions/xnu/blob/main/osfmk/kern/cs_blobs.h).
