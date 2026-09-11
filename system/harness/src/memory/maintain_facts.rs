//! Fact embeddings: facts_vec was created by Plan 2 and never written
//! (assessment: dead schema; facts recall keyword-only). Embed the canonical
//! "subject predicate object" rendering — that's what recall queries match.

use rusqlite::Connection;
use std::path::Path;

/// Backfill `facts_vec` from live facts. Tombstoned facts leave the index
/// first; only facts missing a vector get embedded (idempotent). Returns the
/// number of facts embedded this run.
///
/// The embedder is only constructed when there is pending work, so the
/// sweep + no-op path never pays the model cold-load. `hex_dir` resolves
/// the fastembed cache (`hex_dir/.fastembed_cache`).
pub fn backfill(conn: &Connection, hex_dir: &Path) -> anyhow::Result<usize> {
    let mut embedder = None;
    backfill_with(conn, |texts| {
        if embedder.is_none() {
            embedder = Some(super::embed::Embedder::new(hex_dir)?);
        }
        embedder
            .as_ref()
            .expect("embedder initialized above")
            .embed_documents(texts)
    })
}

/// Run the idempotent fact sweep with an injected document embedder.
///
/// The production wrapper above constructs the real ONNX embedder only when
/// the sweep has pending work. Tests inject a deterministic backend so the
/// database and idempotence contract does not depend on downloading a model.
fn backfill_with<F>(conn: &Connection, mut embed_documents: F) -> anyhow::Result<usize>
where
    F: FnMut(&[String]) -> anyhow::Result<Vec<Vec<f32>>>,
{
    // tombstoned (or deleted) facts must leave the index first
    conn.execute(
        "DELETE FROM facts_vec WHERE fact_id NOT IN
            (SELECT id FROM facts WHERE tombstone = 0)",
        [],
    )?;
    // facts.id is TEXT (ULID) — no CAST needed, and the id can NOT be parsed
    // as an integer; joins on facts.id stay textual throughout.
    let mut stmt = conn.prepare(
        "SELECT f.id, f.subject || ' ' || f.predicate || ' ' || f.object
           FROM facts f
          WHERE f.tombstone = 0
            AND f.id NOT IN (SELECT fact_id FROM facts_vec)",
    )?;
    let rows: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .filter_map(|r| r.ok())
        .collect();
    if rows.is_empty() {
        return Ok(0);
    }
    let mut done = 0;
    // Batch of 8 mirrors index.rs EMBED_BATCH (OBS-019: bounds the per-call
    // ONNX working set).
    for batch in rows.chunks(8) {
        let texts: Vec<String> = batch.iter().map(|(_, t)| t.clone()).collect();
        // Facts are corpus entries — document side of the asymmetric model,
        // matching the chunk pipeline (index.rs). Maintenance ctx: fail loud.
        let vecs = embed_documents(&texts)?;
        for ((id, _), vec) in batch.iter().zip(vecs) {
            // serialize the embedding EXACTLY as vector::insert_vec does for
            // vec_chunks — shared helper, no second serializer.
            super::vector::insert_fact_vec(conn, id, &vec)?;
            done += 1;
        }
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Connection {
        crate::memory::vector::register_sqlite_vec();
        let c = Connection::open_in_memory().unwrap();
        crate::memory::schema::apply_plan1_baseline_for_test(&c).unwrap();
        crate::memory::schema::apply_plan2(&c).unwrap();
        c
    }

    fn insert_fact(c: &Connection, id: &str, object: &str, tombstone: i64) {
        c.execute(
            "INSERT INTO facts (id,subject,predicate,object,importance,created_at,updated_at,tombstone)
             VALUES (?1,'project:hex','uses',?2,0.8,'2026-06-11','2026-06-11',?3)",
            rusqlite::params![id, object, tombstone],
        )
        .unwrap();
    }

    /// Plan Task 11 Step 1: tempdir-style DB with 2 live facts + 1 tombstoned
    /// → backfill embeds exactly the 2 live ones; a re-run backfills 0
    /// (idempotent). The fake backend keeps this database contract test
    /// deterministic and leaves real-model coverage in `embed.rs`.
    #[test]
    fn backfill_embeds_live_facts_only_and_is_idempotent() {
        let c = fixture();
        insert_fact(&c, "f-live-1", "sqlite-vec for the vector store", 0);
        insert_fact(&c, "f-live-2", "fastembed for embeddings", 0);
        insert_fact(&c, "f-dead-1", "an abandoned approach", 1);

        let n = backfill_with(&c, |texts| {
            Ok(texts
                .iter()
                .map(|_| vec![0.25f32; crate::memory::vector::EMBED_DIM])
                .collect())
        })
        .unwrap();
        assert_eq!(n, 2, "both live facts must be embedded");
        let count: i64 = c
            .query_row("SELECT COUNT(*) FROM facts_vec", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2, "facts_vec holds exactly the live facts");
        let dead: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM facts_vec WHERE fact_id = 'f-dead-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(dead, 0, "tombstoned fact must not be embedded");

        let n2 = backfill_with(&c, |_| {
            panic!("the deterministic backend must not run on an idempotent re-run")
        })
        .unwrap();
        assert_eq!(n2, 0, "re-run must backfill nothing (idempotent)");
        let count2: i64 = c
            .query_row("SELECT COUNT(*) FROM facts_vec", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count2, 2);
    }

    /// The tombstone sweep removes stale vectors, and the no-pending path
    /// never constructs the embedder (hex_dir intentionally bogus — hermetic).
    #[test]
    fn backfill_sweeps_tombstoned_vectors_without_loading_embedder() {
        let c = fixture();
        insert_fact(&c, "f-dead-1", "stale knowledge", 1);
        let v = vec![0.5f32; crate::memory::vector::EMBED_DIM];
        crate::memory::vector::insert_fact_vec(&c, "f-dead-1", &v).unwrap();

        let n_real = backfill(&c, Path::new("/nonexistent-hex-dir")).unwrap();
        assert_eq!(
            n_real, 0,
            "the production wrapper must not load a model when idle"
        );

        let n = backfill_with(&c, |_| {
            panic!("the deterministic backend must not run with no pending facts")
        })
        .unwrap();
        assert_eq!(n, 0, "nothing live to embed");
        let count: i64 = c
            .query_row("SELECT COUNT(*) FROM facts_vec", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "tombstoned fact's vector must be swept");
    }
}
