//! Embedding: wraps `fastembed` (nomic-embed-text-v1.5, ONNX/CPU). One
//! `Embedder` holds a resident model; construction pays the ~1.6 s cold-load
//! once. nomic-v1.5 is an asymmetric model — documents and queries get
//! different task prefixes.

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use std::path::Path;
use std::time::{Duration, SystemTime};

// fastembed 4.x does not auto-apply nomic task prefixes — we add them here.
const DOC_PREFIX: &str = "search_document: ";
const QUERY_PREFIX: &str = "search_query: ";

/// Read current resident set size (RSS) in MB on Linux via /proc/self/statm.
/// Returns None on non-Linux or read failure. Used by OBS-019 diagnosis to
/// pinpoint where memory blows up during indexing.
pub fn rss_mb() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let resident_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        // Page size on Linux is typically 4096; query via sysconf if available.
        let page_size: u64 = 4096;
        Some(resident_pages * page_size / (1024 * 1024))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Emit an RSS checkpoint to stderr. Opt-in via `HEX_RSS_LOG`.
pub fn log_rss(label: &str) {
    if std::env::var_os("HEX_RSS_LOG").is_none() {
        return; // diagnostic noise on every search otherwise — opt-in only
    }
    match rss_mb() {
        Some(mb) => eprintln!("[rss] {label}: {mb} MB"),
        None => eprintln!("[rss] {label}: (unavailable on this platform)"),
    }
}

/// Remove stale hf-hub `.lock` files under the fastembed cache. A SIGKILLed
/// download/load leaves locks that block the next `TextEmbedding::try_new`. We
/// only remove locks older than 60s so an actively-downloading sibling process
/// (rare: worker cron overlapping a manual run) isn't disturbed.
fn clear_stale_locks(cache_dir: &Path) {
    if !cache_dir.is_dir() {
        return;
    }
    let now = SystemTime::now();
    for entry in walkdir::WalkDir::new(cache_dir).into_iter().flatten() {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) != Some("lock") {
            continue;
        }
        let stale = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|mtime| now.duration_since(mtime).ok())
            .map(|age| age > Duration::from_secs(60))
            .unwrap_or(true);
        if stale && std::fs::remove_file(p).is_ok() {
            eprintln!("hex memory: cleared stale fastembed lock {}", p.display());
        }
    }
}

pub struct Embedder {
    model: TextEmbedding,
}

// Test-only construction probe. Increments on every `Embedder::new` call, per
// calling thread. Used by `recall_path_constructs_no_embedder` (spec Tj0b203yv)
// to assert the UserPromptSubmit hot path is *structurally* unable to construct
// an Embedder — a counter, not wall-clock timing. Thread-local so parallel
// tests that legitimately construct an embedder (e.g. CLI-search paths) do not
// race with the hot-path assertion.
#[cfg(test)]
thread_local! {
    pub static EMBEDDER_CONSTRUCTIONS_THREAD: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

impl Embedder {
    /// Load the model. On first ever run fastembed downloads the ONNX weights
    /// (~hundreds of MB) into its cache; afterwards this is a local load.
    ///
    /// OBS-019 fix: cap ONNX Runtime intra/inter-op threads to 1 BEFORE
    /// loading the model. fastembed 4.x's InitOptions doesn't expose ORT
    /// session options, so we set the env vars ort reads at module init.
    /// Without this cap, ort defaults to `num_cpus`, and a multi-thread
    /// embed of a batch of N chunks allocates N × per-thread × per-layer
    /// activation tensors — easily 2+ GB on the 70-chunk CLAUDE.md batch,
    /// which OOM-killed the process in the 4 GB Docker test container.
    /// See evolution/obs-019-diagnosis.md for the full RSS trace.
    pub fn new(hex_root: &Path) -> anyhow::Result<Self> {
        #[cfg(test)]
        EMBEDDER_CONSTRUCTIONS_THREAD.with(|c| c.set(c.get() + 1));
        std::env::set_var("ORT_NUM_THREADS", "1");
        std::env::set_var("OMP_NUM_THREADS", "1");
        // Absolute cache dir under $HEX_DIR so embedding works from ANY cwd
        // (fastembed defaults to a cwd-relative `.fastembed_cache`; a worker
        // running from `/` would otherwise miss the cache and fail to load).
        let cache_dir = hex_root.join(".fastembed_cache");
        // OBS-019 self-heal: a SIGKILLed index run leaves hf-hub `.lock` files
        // that block TextEmbedding::try_new ("Failed to retrieve onnx/model.onnx")
        // even when the model is fully cached. Clear stale ones first.
        clear_stale_locks(&cache_dir);
        log_rss("embedder pre-new");
        let model = TextEmbedding::try_new(
            InitOptions::new(EmbeddingModel::NomicEmbedTextV15).with_cache_dir(cache_dir),
        )?;
        log_rss("embedder post-new");
        Ok(Self { model })
    }

    /// Embed corpus chunks (document side).
    pub fn embed_documents(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        let prefixed: Vec<String> = texts.iter().map(|t| format!("{DOC_PREFIX}{t}")).collect();
        self.model.embed(prefixed, None)
    }

    /// Embed a single search query (query side).
    pub fn embed_query(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let mut out = self
            .model
            .embed(vec![format!("{QUERY_PREFIX}{text}")], None)?;
        out.pop()
            .ok_or_else(|| anyhow::anyhow!("fastembed returned no embedding for query"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn clear_stale_locks_removes_old_but_keeps_fresh() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join(".fastembed_cache/models--x/blobs");
        std::fs::create_dir_all(&cache).unwrap();
        let stale = cache.join("abc.lock");
        let fresh = cache.join("def.lock");
        let keep = cache.join("model.onnx");
        std::fs::write(&stale, "").unwrap();
        std::fs::write(&fresh, "").unwrap();
        std::fs::write(&keep, "data").unwrap();
        // Backdate the stale lock to 10 minutes ago.
        let old = SystemTime::now() - Duration::from_secs(600);
        filetime::set_file_mtime(&stale, filetime::FileTime::from_system_time(old)).unwrap();

        clear_stale_locks(&tmp.path().join(".fastembed_cache"));

        assert!(!stale.exists(), "stale lock should be removed");
        assert!(fresh.exists(), "fresh lock (<60s) should be kept");
        assert!(keep.exists(), "non-lock files untouched");
    }

    #[test]
    fn clear_stale_locks_noop_on_missing_dir() {
        clear_stale_locks(Path::new("/tmp/does-not-exist-hex-fastembed"));
    }

    #[test]
    #[ignore = "requires the ONNX embedding model; run with --run-ignored all"]
    fn embeds_at_768_dimensions() {
        let e = Embedder::new(Path::new(".")).unwrap();
        let q = e
            .embed_query("what did we decide about the memory schema")
            .unwrap();
        assert_eq!(q.len(), super::super::vector::EMBED_DIM);

        let docs = e
            .embed_documents(&["hex is an AI operating layer".to_string()])
            .unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].len(), super::super::vector::EMBED_DIM);
    }
}
