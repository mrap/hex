// The `hex memory` CLI boundary test binary.
//
// Covers three things at the CLI boundary:
//   - the `hex memory consolidate` orchestrator, with its `full` and `quick`
//     subcommands (task Tx8a72zfh). Quick mode must run Layer 1
//     (doctor::consolidate) + Layer 2 (memory::consolidate) deterministically
//     with no network. Full mode must be wired (help lists it) but is NOT
//     executed here - we never make a live LLM/provider call in tests.
//   - `hex memory parse-transcripts` visibility: hidden from `hex memory --help`
//     but still callable directly (cron and internal invocations rely on it).
//   - the `--max` flag stays advertised on the `hex memory` subcommands that
//     opt out of background-priority self-throttling.

use std::fs;
use std::process::Command;
use tempfile::TempDir;

fn build_fake_hex_dir() -> TempDir {
    let dir = tempfile::tempdir().expect("create tempdir");
    let p = dir.path();

    fs::write(p.join("CLAUDE.md"), "").unwrap();

    let evo = p.join("evolution");
    fs::create_dir_all(&evo).unwrap();
    fs::write(evo.join("observations.md"), "").unwrap();
    fs::write(evo.join("suggestions.md"), "").unwrap();
    fs::write(evo.join("changelog.md"), "").unwrap();

    fs::create_dir_all(p.join("projects")).unwrap();

    let me = p.join("me");
    fs::create_dir_all(&me).unwrap();
    fs::write(me.join("learnings.md"), "").unwrap();

    dir
}

#[test]
fn consolidate_help_lists_full_and_quick_modes() {
    let bin = env!("CARGO_BIN_EXE_hex");
    let output = Command::new(bin)
        .args(["memory", "consolidate", "--help"])
        .output()
        .expect("hex binary must run");

    assert!(
        output.status.success(),
        "`hex memory consolidate --help` must succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let help = String::from_utf8_lossy(&output.stdout).to_lowercase();
    assert!(
        help.contains("full"),
        "help must list 'full' mode; got:\n{help}"
    );
    assert!(
        help.contains("quick"),
        "help must list 'quick' mode; got:\n{help}"
    );
}

#[test]
fn consolidate_quick_runs_deterministically_with_no_network() {
    let hex_dir = build_fake_hex_dir();
    let bin = env!("CARGO_BIN_EXE_hex");

    let output = Command::new(bin)
        .args(["memory", "consolidate", "quick"])
        .env("HEX_DIR", hex_dir.path())
        // Force any accidental provider call to fail loudly - quick must not need it.
        .env_remove("OPENROUTER_API_KEY")
        .output()
        .expect("hex binary must run");

    let code = output.status.code().unwrap_or(2);
    assert!(
        code == 0 || code == 1,
        "unexpected exit code {code} from `hex memory consolidate quick`; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Layer 1 must write the structural log.
    let log_path = hex_dir
        .path()
        .join("evolution")
        .join("consolidation-latest.log");
    assert!(
        log_path.exists(),
        "consolidation-latest.log must be written by quick mode (Layer 1)"
    );

    // Quick must NOT have written an LLM audit file (that's full-only, Layer 3).
    let evo = hex_dir.path().join("evolution");
    if let Ok(rd) = fs::read_dir(&evo) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            assert!(
                !name.starts_with("consolidation-audit-"),
                "quick mode must NOT write a consolidation-audit-*.md file (LLM-only); found: {name}"
            );
        }
    }

    // The log must carry the structural report header, not just exist.
    let log = fs::read_to_string(&log_path).expect("read consolidation-latest.log");
    assert!(
        log.contains("Consolidation Report"),
        "consolidation-latest.log must contain 'Consolidation Report'; got:\n{log}"
    );
}

// `hex memory parse-transcripts` must be hidden from the user-facing CLI
// (not listed under `hex memory --help`) but still callable directly
// (cron + internal invocations rely on it).
//
// Moved here from the removed parse_transcripts_hidden.rs (task Tfgrc2gny).

#[test]
fn parse_transcripts_not_listed_in_memory_help() {
    let bin = env!("CARGO_BIN_EXE_hex");
    let out = Command::new(bin)
        .args(["memory", "--help"])
        .output()
        .expect("run hex memory --help");
    assert!(
        out.status.success(),
        "`hex memory --help` failed. stdout: {} stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(
        !help.contains("parse-transcripts"),
        "`parse-transcripts` must be hidden from `hex memory --help`; \
         got listing:\n{help}"
    );
}

#[test]
fn parse_transcripts_still_callable() {
    let bin = env!("CARGO_BIN_EXE_hex");
    let out = Command::new(bin)
        .args(["memory", "parse-transcripts", "--help"])
        .output()
        .expect("run hex memory parse-transcripts --help");
    assert!(
        out.status.success(),
        "`hex memory parse-transcripts --help` must still succeed (hidden, not removed); \
         stdout: {} stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

// `hex memory consolidate {quick,full}` and `hex memory index` must each
// accept a `--max` flag that opts out of background-priority self-throttling
// (task Tr7pzxkk5). Table-driven so a dropped flag on any one subcommand
// names that subcommand in the failure, not just "one of them broke".
//
// Moved here from the removed throttle_max_flag.rs.
#[test]
fn memory_subcommands_advertise_max_flag() {
    let bin = env!("CARGO_BIN_EXE_hex");
    let subcommands: [&[&str]; 3] = [
        &["memory", "consolidate", "full"],
        &["memory", "consolidate", "quick"],
        &["memory", "index"],
    ];

    for args in subcommands {
        let help_args: Vec<&str> = args.iter().copied().chain(["--help"]).collect();
        let out = Command::new(bin)
            .args(&help_args)
            .output()
            .unwrap_or_else(|e| panic!("run `hex {}`: {e}", args.join(" ")));
        assert!(
            out.status.success(),
            "`hex {} --help` must succeed. stderr: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr),
        );
        let help = String::from_utf8_lossy(&out.stdout);
        assert!(
            help.contains("--max"),
            "`hex {} --help` must list `--max` flag; got:\n{help}",
            args.join(" "),
        );
    }
}
