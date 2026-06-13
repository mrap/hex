//! Offset-tracking tail reader. Given a path and a byte offset, returns the
//! complete new lines appended since that offset plus the advanced offset.
//! Detects rotation/truncation two ways: file shorter than the offset (truncate),
//! OR the inode changed (logrotate move-and-recreate, even when the new file has
//! already grown past the old offset). Either case restarts from byte 0.
//!
//! Pure filesystem I/O — no `notify`, no emit — so it is deterministically
//! testable with temp files, without depending on FS-event timing.

use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Sentinel: "seek to EOF on the first read" (i.e. only new lines, no checkpoint).
const SEEK_EOF: u64 = u64::MAX;

pub struct TailReader {
    path: PathBuf,
    offset: u64,
    /// Inode of the file last read, to detect rotation that size alone misses
    /// (a new inode at the same path that already grew past `offset`).
    last_inode: Option<u64>,
}

/// Inode of an open file, when the platform exposes one (Unix). `None` elsewhere,
/// where rotation falls back to the size-shrink check only.
fn inode_of(f: &File) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        f.metadata().ok().map(|m| m.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = f;
        None
    }
}

impl TailReader {
    /// `from_start = true` reads the whole file from byte 0; `false` starts at EOF
    /// on the first read (only newly-appended lines are seen) unless a checkpoint
    /// is restored via [`TailReader::set_offset`].
    pub fn new(path: impl Into<PathBuf>, from_start: bool) -> Self {
        TailReader {
            path: path.into(),
            offset: if from_start { 0 } else { SEEK_EOF },
            last_inode: None,
        }
    }

    /// Current byte offset (the checkpoint to persist). The "seek to EOF" sentinel
    /// reports as 0 (nothing meaningful to persist before the first read).
    pub fn offset(&self) -> u64 {
        if self.offset == SEEK_EOF {
            0
        } else {
            self.offset
        }
    }

    /// Restore a persisted byte offset (resume across restarts without replay/drop).
    pub fn set_offset(&mut self, offset: u64) {
        self.offset = offset;
    }

    /// The parent directory of the tailed file (watched for rotation/creation).
    pub fn parent_dir(&self) -> Option<&Path> {
        self.path.parent()
    }

    /// Read all complete new lines since the current offset. A trailing partial
    /// line (no newline yet) is NOT returned and NOT counted — it is re-read once
    /// completed. A missing file yields `Ok(vec![])` (not an error: the log may not
    /// exist yet). Rotation (size shrink OR inode change) restarts from byte 0.
    pub fn read_new(&mut self) -> std::io::Result<Vec<String>> {
        let f = match File::open(&self.path) {
            Ok(f) => f,
            Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let len = f.metadata()?.len();
        let inode = inode_of(&f);

        // First read with no checkpoint and from_start=false: start at EOF.
        if self.offset == SEEK_EOF {
            self.offset = len;
            self.last_inode = inode;
            return Ok(Vec::new());
        }

        // Rotation detection:
        //  - inode changed (logrotate move+recreate) → a different file at the path,
        //  - OR the file shrank below our offset (copytruncate / truncation).
        // Either way, restart from the beginning of the current file.
        let inode_changed = match (self.last_inode, inode) {
            (Some(prev), Some(cur)) => prev != cur,
            _ => false, // unknown on either side → fall back to size check only
        };
        if inode_changed || len < self.offset {
            self.offset = 0;
        }
        self.last_inode = inode;

        if len == self.offset {
            return Ok(Vec::new());
        }

        let mut file = f;
        file.seek(SeekFrom::Start(self.offset))?;
        let mut reader = BufReader::new(file);
        let mut lines = Vec::new();
        let mut consumed: u64 = 0;
        loop {
            let mut buf = Vec::new();
            // `read_until` is byte-oriented (no UTF-8 assumption) and fills `buf`
            // up to and including the delimiter — exactly the semantics we want.
            let n = reader.read_until(b'\n', &mut buf)?;
            if n == 0 {
                break;
            }
            if buf.last() == Some(&b'\n') {
                // Complete line — count it and strip the trailing newline(s).
                consumed += n as u64;
                while matches!(buf.last(), Some(b'\n') | Some(b'\r')) {
                    buf.pop();
                }
                lines.push(String::from_utf8_lossy(&buf).into_owned());
            } else {
                // Partial trailing line (no newline yet) — leave it for next read.
                break;
            }
        }
        self.offset += consumed;
        Ok(lines)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp() -> PathBuf {
        let mut p = std::env::temp_dir();
        let uniq = format!(
            "hex-logtail-test-{}-{}.log",
            std::process::id(),
            // monotonic-ish unique suffix without external crates
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        p.push(uniq);
        p
    }

    #[test]
    fn appends_advance_offset_and_only_complete_lines() {
        let path = tmp();
        std::fs::write(&path, b"line one\nline two\n").unwrap();
        let mut r = TailReader::new(&path, true);
        let lines = r.read_new().unwrap();
        assert_eq!(lines, vec!["line one".to_string(), "line two".to_string()]);
        let off = r.offset();
        assert_eq!(off, 18);

        // Append a partial line (no newline): not returned yet.
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            f.write_all(b"partial").unwrap();
        }
        assert!(r.read_new().unwrap().is_empty());
        assert_eq!(r.offset(), off, "partial line must not advance the offset");

        // Complete it: now it appears.
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            f.write_all(b" done\n").unwrap();
        }
        assert_eq!(r.read_new().unwrap(), vec!["partial done".to_string()]);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn from_eof_skips_existing_then_reads_new() {
        let path = tmp();
        std::fs::write(&path, b"old\n").unwrap();
        let mut r = TailReader::new(&path, false);
        // First read seeks to EOF — existing content is skipped.
        assert!(r.read_new().unwrap().is_empty());
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            f.write_all(b"new\n").unwrap();
        }
        assert_eq!(r.read_new().unwrap(), vec!["new".to_string()]);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn truncation_restarts_from_zero() {
        let path = tmp();
        std::fs::write(&path, b"a\nb\n").unwrap();
        let mut r = TailReader::new(&path, true);
        assert_eq!(r.read_new().unwrap().len(), 2);
        // Same inode, shorter content (copytruncate) → len (2) < offset (4) → restart.
        let f = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        f.set_len(0).unwrap();
        drop(f);
        std::fs::write(&path, b"c\n").unwrap();
        assert_eq!(r.read_new().unwrap(), vec!["c".to_string()]);
        std::fs::remove_file(&path).ok();
    }

    #[cfg(unix)]
    #[test]
    fn inode_change_restarts_even_when_new_file_is_larger() {
        // The size-only check misses this: the rotated file is LONGER than the old
        // offset, so only the inode comparison catches it.
        let path = tmp();
        std::fs::write(&path, b"x\n").unwrap(); // offset will be 2
        let mut r = TailReader::new(&path, true);
        assert_eq!(r.read_new().unwrap(), vec!["x".to_string()]);
        assert_eq!(r.offset(), 2);

        // Rotate: remove and recreate at the same path (new inode), already grown
        // past the old offset of 2.
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"alpha\nbeta\n").unwrap(); // len 11 > offset 2
        let lines = r.read_new().unwrap();
        assert_eq!(
            lines,
            vec!["alpha".to_string(), "beta".to_string()],
            "inode change must restart from 0 even though the new file is larger"
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn missing_file_is_not_an_error() {
        let path = tmp(); // never created
        let mut r = TailReader::new(&path, true);
        assert!(r.read_new().unwrap().is_empty());
    }

    #[test]
    fn strips_crlf() {
        let path = tmp();
        std::fs::write(&path, b"win\r\nunix\n").unwrap();
        let mut r = TailReader::new(&path, true);
        assert_eq!(
            r.read_new().unwrap(),
            vec!["win".to_string(), "unix".to_string()]
        );
        std::fs::remove_file(&path).ok();
    }
}
