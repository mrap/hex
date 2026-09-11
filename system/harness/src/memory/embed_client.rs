//! Query-side embedding over a local unix socket — the low-latency path that
//! turns on `facts_recall`'s semantic KNN arm (spec Sdnap37he, task Ttrmaca6q).
//!
//! Chosen option (b) from `docs/research/2026-08-19-recall-vector-arm.md`: the
//! long-running hex engine holds ONE resident [`Embedder`](super::embed::Embedder)
//! and answers query-embedding requests over a local unix socket
//! (`hex memory embed-serve`, wired via [`serve_with`]). The recall CLI is a
//! fresh OS process per user message, so cold-loading the ~522 MB nomic model
//! in-process is off-budget (measured 13–15 s under load; ~1.6 s quiet floor —
//! see the research memo §3). Asking the resident endpoint pays the cold-load
//! ONCE and leaves the recall hot path with only a single query-side forward
//! pass behind a sub-millisecond socket round-trip.
//!
//! ## Contract (SO S6 — no quiet failures)
//!
//! * DEFAULT OFF: [`query_vector`] returns `None` immediately when the arm is
//!   disabled — no socket is touched, so recall is byte-identical to BM25-only.
//! * LOUD + BOUNDED fallback: any failure (missing socket, dead/slow endpoint,
//!   malformed reply, timeout) emits a stderr WARN and returns `None` within
//!   `timeout_ms`. It never errors recall and never adds unbounded latency —
//!   the whole embed step runs on a worker thread the caller waits on for at
//!   most `timeout_ms` (a hung endpoint cannot stall the hot path).
//! * NO NETWORK: the endpoint is a local `AF_UNIX` socket only.
//!
//! ## Wire protocol (newline-framed request, fixed-width response)
//!
//! ```text
//! → request:  <query-utf8-bytes> b'\n'
//! ← response: EMBED_DIM little-endian f32  (768 * 4 = 3072 bytes)
//! ```
//!
//! Both sides live in this file ([`try_roundtrip`] reads, [`serve_with`]
//! writes) so the framing has exactly one in-tree reader and one in-tree writer
//! and cannot drift.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use super::recall_config::VectorArm;
use super::vector::EMBED_DIM;

/// Number of response bytes on the wire: `EMBED_DIM` little-endian f32.
const RESPONSE_BYTES: usize = EMBED_DIM * 4;

/// Resolve the configured socket path against `$HEX_DIR`. Absolute paths are
/// used verbatim; relative paths resolve under `hex_root`.
fn resolve_socket_path(hex_root: &Path, socket_path: &str) -> PathBuf {
    let p = Path::new(socket_path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        hex_root.join(p)
    }
}

/// Obtain the query embedding for the facts KNN arm, or `None` to run BM25-only.
///
/// Returns `None` immediately (touching nothing) when `cfg.enabled` is false —
/// the byte-identical default. When enabled, performs the socket round-trip on
/// a worker thread and waits at most `cfg.timeout_ms`; on any failure or
/// timeout it emits a stderr WARN and returns `None`. It never panics, never
/// errors the recall, and never blocks longer than the configured bound.
pub fn query_vector(hex_root: &Path, cfg: &VectorArm, query: &str) -> Option<Vec<f32>> {
    if !cfg.enabled {
        // DEFAULT OFF: no query vector, no socket, byte-identical BM25-only.
        return None;
    }

    let timeout = Duration::from_millis(cfg.timeout_ms.max(1));
    let sock = resolve_socket_path(hex_root, &cfg.socket_path);
    let query = query.to_string();

    // Hard wall-clock ceiling: the round-trip runs on a worker thread and the
    // caller waits at most `timeout`. A hung endpoint cannot add unbounded
    // latency to the hot recall path — we abandon the thread and fall back.
    // The recall CLI is short-lived, so an abandoned thread dies with it.
    let (tx, rx) = mpsc::channel();
    let sock_for_thread = sock.clone();
    std::thread::spawn(move || {
        let _ = tx.send(try_roundtrip(&sock_for_thread, &query, timeout));
    });

    match rx.recv_timeout(timeout) {
        Ok(Ok(vec)) => Some(vec),
        Ok(Err(reason)) => {
            eprintln!("{}", fallback_warn_line(&sock, &reason));
            None
        }
        Err(_) => {
            eprintln!(
                "{}",
                fallback_warn_line(&sock, &format!("timed out after {timeout:?}"))
            );
            None
        }
    }
}

/// Build the loud BM25-only fallback WARN line (SO S6 — no quiet failures).
///
/// Pure and `pub(crate)` so a unit test can assert the fallback is genuinely
/// LOUD — carries `WARN` and names the degraded mode — without capturing
/// stderr. Both `eprintln!` sites in [`query_vector`] render through this, so
/// the on-the-wire wording of the stderr WARN cannot drift from what the test
/// pins. `detail` is the failure cause (`try_roundtrip`'s reason, or the
/// timeout note).
pub(crate) fn fallback_warn_line(sock: &Path, detail: &str) -> String {
    format!(
        "[recall vector arm] WARN embed endpoint {} unavailable, \
         falling back to BM25-only: {detail}",
        sock.display()
    )
}

/// One request/response round-trip against the embed socket. Returns the query
/// vector, or an `Err` whose message is the WARN reason [`query_vector`] logs.
///
/// `pub(crate)` so the loud-fallback unit test can assert the failure is
/// reported (a non-empty reason) without capturing stderr.
pub(crate) fn try_roundtrip(
    sock: &Path,
    query: &str,
    timeout: Duration,
) -> Result<Vec<f32>, String> {
    let mut stream = UnixStream::connect(sock).map_err(|e| format!("connect failed: {e}"))?;
    // Per-syscall timeouts so the worker thread itself cannot block forever
    // (belt-and-suspenders with the caller-side `recv_timeout` ceiling).
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| format!("set_read_timeout failed: {e}"))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|e| format!("set_write_timeout failed: {e}"))?;

    let mut req = query.as_bytes().to_vec();
    req.push(b'\n');
    stream
        .write_all(&req)
        .map_err(|e| format!("write failed: {e}"))?;
    stream.flush().map_err(|e| format!("flush failed: {e}"))?;

    let mut buf = vec![0u8; RESPONSE_BYTES];
    stream
        .read_exact(&mut buf)
        .map_err(|e| format!("read failed (want {RESPONSE_BYTES} bytes): {e}"))?;

    let vec: Vec<f32> = buf
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    if vec.len() != EMBED_DIM {
        return Err(format!(
            "malformed reply: {} floats, expected {EMBED_DIM}",
            vec.len()
        ));
    }
    Ok(vec)
}

/// Serve query embeddings over a unix socket at `socket_path` (option (b)'s
/// resident endpoint). `embed` produces the query vector for one query — the
/// CLI wires it to the resident [`Embedder`](super::embed::Embedder); tests
/// pass a fake so the framing is exercised without cold-loading the model.
///
/// Blocks forever, serving one connection at a time (the recall CLI issues one
/// short request per invocation). Stale socket files are removed before bind.
pub fn serve_with<F>(socket_path: &Path, embed: F) -> std::io::Result<()>
where
    F: Fn(&str) -> Option<Vec<f32>>,
{
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // A leftover socket file from a previous run makes bind() fail with
    // EADDRINUSE even though nothing is listening — but unlinking a LIVE
    // server's socket would silently orphan it (a second resident embedder,
    // unreachable, still holding the ~522 MB model). Probe first: only a
    // confirmed-dead socket is removed; a live one is a loud refusal.
    if socket_path.exists() {
        match UnixStream::connect(socket_path) {
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    format!("embed-serve already listening on {}", socket_path.display()),
                ));
            }
            Err(_) => {
                let _ = std::fs::remove_file(socket_path);
            }
        }
    }
    let listener = UnixListener::bind(socket_path)?;
    eprintln!(
        "[embed-serve] listening on {} (dim {EMBED_DIM})",
        socket_path.display()
    );
    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                if let Err(e) = handle_conn(stream, &embed) {
                    // One bad client must not take the server down (SO S6: loud,
                    // not fatal).
                    eprintln!("[embed-serve] WARN connection error: {e}");
                }
            }
            Err(e) => eprintln!("[embed-serve] WARN accept error: {e}"),
        }
    }
    Ok(())
}

/// Per-connection I/O ceiling on the ACCEPTED stream. The serve loop handles
/// one connection at a time, so a client that stalls mid-request would
/// otherwise wedge the accept loop forever (every future query then pays its
/// client-side timeout and falls back, silently, for good).
const CONN_IO_TIMEOUT: Duration = Duration::from_secs(2);

/// Longest accepted request line. Recall queries are single chat messages;
/// anything larger is a bug or abuse and is refused before it can grow the
/// server's memory (the reader is capped, not just checked after the fact).
const MAX_QUERY_BYTES: u64 = 64 * 1024;

/// Read one newline-framed query, embed it, and write the fixed-width response.
fn handle_conn<F>(stream: UnixStream, embed: &F) -> std::io::Result<()>
where
    F: Fn(&str) -> Option<Vec<f32>>,
{
    stream.set_read_timeout(Some(CONN_IO_TIMEOUT))?;
    stream.set_write_timeout(Some(CONN_IO_TIMEOUT))?;
    let mut line = String::new();
    BufReader::new((&stream).take(MAX_QUERY_BYTES)).read_line(&mut line)?;
    // A capped read that never saw the newline is an oversized or truncated
    // request (EOF mid-line) — refuse it rather than embedding a fragment.
    if !line.ends_with('\n') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("request not newline-terminated within {MAX_QUERY_BYTES} bytes"),
        ));
    }
    let query = line.trim_end_matches(['\r', '\n']);

    let vec = embed(query).unwrap_or_default();
    let mut out = Vec::with_capacity(RESPONSE_BYTES);
    // A conforming reply is exactly EMBED_DIM floats; an embed miss writes an
    // empty body so the client's read_exact fails loudly rather than hanging.
    if vec.len() == EMBED_DIM {
        for f in &vec {
            out.extend_from_slice(&f.to_le_bytes());
        }
    }
    (&stream).write_all(&out)?;
    (&stream).flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// A fake conforming server: reads one query line, replies with a constant
    /// `EMBED_DIM`-float vector. Returns the bound socket path + join handle.
    fn spawn_fake_server(dir: &Path, fill: f32) -> PathBuf {
        let sock = dir.join("embed.sock");
        let s2 = sock.clone();
        std::thread::spawn(move || {
            serve_with(&s2, move |_q| Some(vec![fill; EMBED_DIM])).ok();
        });
        // Wait for the socket to appear (bind happens on the worker thread).
        for _ in 0..200 {
            if sock.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        sock
    }

    #[test]
    fn disabled_arm_returns_none_without_touching_socket() {
        // DEFAULT OFF => None immediately, even if socket_path is set. This is
        // the byte-identical guarantee: off => no query vector => BM25-only.
        let cfg = VectorArm::default();
        assert!(!cfg.enabled);
        let got = query_vector(
            Path::new("/nonexistent"),
            &cfg,
            "a semantic paraphrase with no keyword overlap",
        );
        assert!(got.is_none(), "disabled arm must yield no query vector");
    }

    #[test]
    fn dead_socket_is_loud_bounded_and_bm25_only() {
        // Enabled arm pointing at a socket that does not exist: try_roundtrip
        // must report a WARN reason (loud), and query_vector must return None
        // well within a hard time bound (never hang, never panic, never error).
        let arm = VectorArm {
            enabled: true,
            socket_path: "/nonexistent/definitely-not-a.sock".to_string(),
            timeout_ms: 100,
        };
        // The reason string is what query_vector prints to stderr — assert it
        // is non-empty (loud) without capturing stderr.
        let reason = try_roundtrip(
            Path::new(&arm.socket_path),
            "query",
            Duration::from_millis(arm.timeout_ms),
        )
        .expect_err("dead socket must fail");
        assert!(!reason.is_empty(), "fallback must carry a loud reason");

        let start = Instant::now();
        let got = query_vector(Path::new("/tmp"), &arm, "query text");
        let elapsed = start.elapsed();
        assert!(got.is_none(), "dead socket must fall back to None");
        assert!(
            elapsed < Duration::from_millis(500),
            "fallback must be bounded, took {elapsed:?}"
        );
    }

    #[test]
    fn fallback_warn_line_is_loud() {
        // Verification `fallback-loud-bounded` names a *stderr WARN* explicitly.
        // Both fallback branches in `query_vector` write exactly this string to
        // stderr, so pinning it here proves the degrade is LOUD (SO S6): it
        // carries `WARN`, names the degraded mode (`BM25-only`), the endpoint,
        // and the cause — no quiet swallow.
        let reason_line = fallback_warn_line(
            Path::new("/tmp/embed.sock"),
            "connect failed: No such file or directory (os error 2)",
        );
        assert!(
            reason_line.contains("WARN"),
            "fallback must be loud: {reason_line}"
        );
        assert!(
            reason_line.contains("BM25-only"),
            "fallback must name the degraded mode: {reason_line}"
        );
        assert!(
            reason_line.contains("/tmp/embed.sock"),
            "fallback must name the endpoint: {reason_line}"
        );
        assert!(
            reason_line.contains("connect failed"),
            "fallback must carry the cause: {reason_line}"
        );
        // The timeout branch renders through the same builder, so it is loud too.
        let timeout_line =
            fallback_warn_line(Path::new("/tmp/embed.sock"), "timed out after 150ms");
        assert!(timeout_line.contains("WARN") && timeout_line.contains("BM25-only"));
        assert!(
            timeout_line.contains("timed out"),
            "timeout cause must survive"
        );
    }

    #[test]
    fn hung_endpoint_respects_timeout_bound() {
        // A server that accepts but never replies must not stall recall: the
        // caller-side ceiling abandons it and falls back within the bound.
        let dir = tempfile::TempDir::new().unwrap();
        let sock = dir.path().join("embed.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        std::thread::spawn(move || {
            if let Ok((_stream, _)) = listener.accept() {
                std::thread::sleep(Duration::from_secs(5)); // hang, never write
            }
        });

        let arm = VectorArm {
            enabled: true,
            socket_path: sock.to_string_lossy().into_owned(),
            timeout_ms: 100,
        };
        let start = Instant::now();
        let got = query_vector(dir.path(), &arm, "query");
        let elapsed = start.elapsed();
        assert!(got.is_none(), "hung endpoint must fall back to None");
        assert!(
            elapsed < Duration::from_millis(600),
            "must respect the timeout bound, took {elapsed:?}"
        );
    }

    #[test]
    fn conforming_endpoint_returns_query_vector() {
        // Proves the shipped client path actually works against a server that
        // speaks the protocol — the same protocol task 3's endpoint implements.
        let dir = tempfile::TempDir::new().unwrap();
        let sock = spawn_fake_server(dir.path(), 0.5);
        let arm = VectorArm {
            enabled: true,
            socket_path: sock.to_string_lossy().into_owned(),
            timeout_ms: 2000,
        };
        let got = query_vector(dir.path(), &arm, "hello world");
        assert_eq!(
            got.as_ref().map(|v| v.len()),
            Some(EMBED_DIM),
            "conforming endpoint must return an EMBED_DIM query vector"
        );
        assert!(got.unwrap().iter().all(|&f| (f - 0.5).abs() < 1e-6));
    }

    #[test]
    fn relative_socket_path_resolves_under_hex_root() {
        let got = resolve_socket_path(Path::new("/home/x/hex"), ".hex/run/embed.sock");
        assert_eq!(got, Path::new("/home/x/hex/.hex/run/embed.sock"));
        let abs = resolve_socket_path(Path::new("/home/x/hex"), "/tmp/e.sock");
        assert_eq!(abs, Path::new("/tmp/e.sock"));
    }

    #[test]
    fn stalled_connection_cannot_wedge_the_server() {
        // A client that connects and never completes its newline-framed request
        // must be timed out server-side (CONN_IO_TIMEOUT) so the one-at-a-time
        // accept loop keeps serving. Without the accepted-stream timeouts this
        // test hangs the fake server forever and the legit roundtrip times out.
        let dir = tempfile::TempDir::new().unwrap();
        let sock = spawn_fake_server(dir.path(), 0.25);

        let mut stalled = UnixStream::connect(&sock).unwrap();
        stalled.write_all(b"no newline ever").unwrap(); // then just hold it open

        let arm = VectorArm {
            enabled: true,
            socket_path: sock.to_string_lossy().into_owned(),
            timeout_ms: CONN_IO_TIMEOUT.as_millis() as u64 + 2000,
        };
        let got = query_vector(dir.path(), &arm, "legit query");
        assert_eq!(
            got.as_ref().map(|v| v.len()),
            Some(EMBED_DIM),
            "server must shed the stalled connection and serve the next client"
        );
        drop(stalled);
    }

    #[test]
    fn oversized_request_is_refused_not_buffered() {
        // A request larger than MAX_QUERY_BYTES with no newline must be refused
        // (capped read => InvalidData) without growing server memory, and the
        // server must go on serving conforming clients.
        let dir = tempfile::TempDir::new().unwrap();
        let sock = spawn_fake_server(dir.path(), 0.75);

        {
            let mut abuser = UnixStream::connect(&sock).unwrap();
            let junk = vec![b'x'; (MAX_QUERY_BYTES + 1024) as usize];
            // The server closes the socket at the cap; a write error here is fine.
            let _ = abuser.write_all(&junk);
        }

        let arm = VectorArm {
            enabled: true,
            socket_path: sock.to_string_lossy().into_owned(),
            timeout_ms: 2000,
        };
        let got = query_vector(dir.path(), &arm, "still works");
        assert_eq!(
            got.as_ref().map(|v| v.len()),
            Some(EMBED_DIM),
            "server must survive an oversized request and keep serving"
        );
    }

    #[test]
    fn double_start_refuses_live_socket_but_reclaims_stale_one() {
        // Live socket: a second serve_with must FAIL LOUDLY (AddrInUse), never
        // unlink the live server's socket out from under it (which would orphan
        // a resident embedder silently — S6). Stale socket file: reclaimed.
        let dir = tempfile::TempDir::new().unwrap();
        let sock = spawn_fake_server(dir.path(), 0.1);

        let err = serve_with(&sock, |_q| None)
            .expect_err("second server on a live socket must refuse to start");
        assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);
        // The live server must still be reachable after the refused start.
        let arm = VectorArm {
            enabled: true,
            socket_path: sock.to_string_lossy().into_owned(),
            timeout_ms: 2000,
        };
        assert!(
            query_vector(dir.path(), &arm, "after refused double-start").is_some(),
            "refused double-start must leave the live server untouched"
        );

        // Stale: a bound-then-dropped listener leaves a dead socket file behind;
        // a fresh server must reclaim it (the EADDRINUSE case the unlink is for).
        let stale_dir = tempfile::TempDir::new().unwrap();
        let stale = stale_dir.path().join("stale.sock");
        drop(UnixListener::bind(&stale).unwrap());
        assert!(stale.exists(), "dropped listener must leave a socket file");
        let s2 = stale.clone();
        std::thread::spawn(move || {
            serve_with(&s2, move |_q| Some(vec![0.9; EMBED_DIM])).ok();
        });
        for _ in 0..200 {
            if UnixStream::connect(&stale).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let arm2 = VectorArm {
            enabled: true,
            socket_path: stale.to_string_lossy().into_owned(),
            timeout_ms: 2000,
        };
        assert!(
            query_vector(stale_dir.path(), &arm2, "reclaimed").is_some(),
            "stale socket file must be reclaimed by a fresh server"
        );
    }
}
