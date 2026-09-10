//! Bounded authority-registry resolution for registered memory topics.
//!
//! This module intentionally owns only the registry and direct-source boundary.
//! It does not change generic recall when the registry is absent or unmatched.

use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Read};
#[cfg(test)]
use std::path::PathBuf;
use std::path::{Component, Path};

const REGISTRY_RELATIVE: &str = ".hex/config/memory-authority.toml";
const REGISTRY_LIMIT: u64 = 64 * 1024;
const SOURCE_LIMIT: u64 = 128 * 1024;
const SOURCE_AGGREGATE_LIMIT: usize = 256 * 1024;
const MAX_ENTRIES: usize = 64;
const MAX_MATCHES: usize = 8;
const MAX_SOURCES: usize = 2;
pub(crate) const MAX_EXCERPT_CHARS: usize = 900;
const MAX_PATH_COMPONENTS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthorityStatus {
    Current,
    Historical,
    Unknown,
}

impl AuthorityStatus {
    pub(crate) fn from_db(raw: &str) -> Self {
        match raw {
            "current" => Self::Current,
            "historical" => Self::Historical,
            "unknown" => Self::Unknown,
            other => {
                eprintln!("[memory authority] invalid database authority_status {other:?}; treating as unknown");
                Self::Unknown
            }
        }
    }

    fn from_registry(raw: Option<&str>) -> Result<Self, ()> {
        match raw {
            Some("current") => Ok(Self::Current),
            Some("historical") => Ok(Self::Historical),
            Some("unknown") | None => Ok(Self::Unknown),
            Some(_) => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QueryIntent {
    Current,
    Historical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResolutionKind {
    Absent,
    Unmatched,
    Matched,
    Invalid,
    PrivateOnly,
    Conflict,
    Unsupported,
}

impl ResolutionKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Unmatched => "unmatched",
            Self::Matched => "matched",
            Self::Invalid => "invalid",
            Self::PrivateOnly => "private_only",
            Self::Conflict => "conflict",
            Self::Unsupported => "unsupported",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedSource {
    pub(crate) id: String,
    pub(crate) path: String,
    pub(crate) status: AuthorityStatus,
    pub(crate) effective_date: Option<String>,
    pub(crate) superseded_by: Option<String>,
    pub(crate) excerpt: String,
}

#[derive(Debug, Clone)]
pub(crate) struct Resolution {
    pub(crate) kind: ResolutionKind,
    pub(crate) intent: QueryIntent,
    pub(crate) sources: Vec<ResolvedSource>,
    pub(crate) notice: Option<String>,
}

impl Resolution {
    fn simple(kind: ResolutionKind, intent: QueryIntent, notice: Option<String>) -> Self {
        Self {
            kind,
            intent,
            sources: Vec::new(),
            notice,
        }
    }

    pub(crate) fn preserves_generic(&self) -> bool {
        matches!(
            self.kind,
            ResolutionKind::Absent | ResolutionKind::Unmatched
        )
    }

    pub(crate) fn is_degraded(&self) -> bool {
        matches!(
            self.kind,
            ResolutionKind::Invalid | ResolutionKind::Conflict | ResolutionKind::Unsupported
        )
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    version: u32,
    #[serde(default)]
    sources: Vec<RegistrySource>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistrySource {
    id: String,
    path: String,
    #[serde(default)]
    heading: Option<String>,
    topics: Vec<String>,
    #[serde(default)]
    authority_status: Option<String>,
    #[serde(default)]
    effective_date: Option<String>,
    #[serde(default)]
    superseded_by: Option<String>,
    #[serde(default)]
    private: Option<bool>,
}

#[derive(Debug, Clone)]
struct EligibleSource {
    source: RegistrySource,
    status: AuthorityStatus,
}

fn query_intent(query: &str) -> QueryIntent {
    let tokens = tokens(query);
    if [
        "history",
        "historical",
        "previous",
        "former",
        "superseded",
        "before",
    ]
    .iter()
    .any(|cue| tokens.contains(*cue))
    {
        QueryIntent::Historical
    } else {
        QueryIntent::Current
    }
}

fn tokens(text: &str) -> HashSet<String> {
    text.to_lowercase()
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .collect()
}

fn is_generic_topic_token(token: &str) -> bool {
    matches!(
        token,
        "a" | "an"
            | "and"
            | "are"
            | "as"
            | "at"
            | "be"
            | "by"
            | "for"
            | "from"
            | "how"
            | "in"
            | "is"
            | "it"
            | "of"
            | "on"
            | "or"
            | "that"
            | "the"
            | "this"
            | "to"
            | "what"
            | "when"
            | "where"
            | "which"
            | "who"
            | "why"
            | "with"
    )
}

fn topic_tokens(topic: &str) -> Option<HashSet<String>> {
    let tokens: HashSet<String> = tokens(topic)
        .into_iter()
        .filter(|token| !is_generic_topic_token(token))
        .collect();
    (tokens.len() >= 2).then_some(tokens)
}

fn valid_relative_path(path: &str) -> bool {
    let path = Path::new(path);
    let components: Vec<_> = path.components().collect();
    !path.as_os_str().is_empty()
        && components.len() <= MAX_PATH_COMPONENTS
        && components
            .iter()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn normalize_superseded_by(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|value| !value.is_empty())
}

fn validate_registry(registry: &Registry) -> Result<(), String> {
    if registry.sources.len() > MAX_ENTRIES {
        return Err("registry entry limit".to_owned());
    }
    let mut ids = HashSet::new();
    for source in &registry.sources {
        if source.id.trim().is_empty() || !ids.insert(source.id.as_str()) {
            return Err("invalid registry".to_owned());
        }
        if !valid_relative_path(&source.path)
            || source
                .heading
                .as_ref()
                .is_some_and(|heading| heading.trim().is_empty())
            || source.topics.is_empty()
            || source
                .topics
                .iter()
                .any(|topic| topic_tokens(topic).is_none())
            || AuthorityStatus::from_registry(source.authority_status.as_deref()).is_err()
        {
            return Err("invalid registry".to_owned());
        }
    }
    Ok(())
}

fn effective_sources(
    registry: &Registry,
    matched: Vec<RegistrySource>,
    for_agent: bool,
) -> Result<Vec<EligibleSource>, String> {
    let by_id: HashMap<&str, &RegistrySource> = registry
        .sources
        .iter()
        .map(|source| (source.id.as_str(), source))
        .collect();
    let mut eligible = Vec::new();
    for source in matched {
        if for_agent && source.private != Some(false) {
            continue;
        }
        let raw_status = AuthorityStatus::from_registry(source.authority_status.as_deref())
            .map_err(|_| format!("invalid registry status for {}", source.id))?;
        let status = match normalize_superseded_by(&source.superseded_by) {
            None => raw_status,
            Some(target_id) => {
                let target = by_id
                    .get(target_id)
                    .copied()
                    .ok_or_else(|| format!("invalid registry supersession for {}", source.id))?;
                let target_status =
                    AuthorityStatus::from_registry(target.authority_status.as_deref())
                        .map_err(|_| format!("invalid registry status for {}", target.id))?;
                if target.id == source.id
                    || normalize_superseded_by(&target.superseded_by).is_some()
                    || target_status != AuthorityStatus::Current
                    || (for_agent && target.private != Some(false))
                {
                    return Err(format!("invalid registry supersession for {}", source.id));
                }
                AuthorityStatus::Historical
            }
        };
        eligible.push(EligibleSource { source, status });
    }
    Ok(eligible)
}

pub(crate) fn resolve(hex_root: &Path, query: &str, for_agent: bool) -> Resolution {
    let intent = query_intent(query);
    let registry_bytes = match read_root_relative(
        hex_root,
        Path::new(REGISTRY_RELATIVE),
        REGISTRY_LIMIT,
        ReadPurpose::Registry,
    ) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Resolution::simple(ResolutionKind::Absent, intent, None);
        }
        Err(error) => {
            eprintln!("[memory authority] registry read failed: {error}");
            return Resolution::simple(
                ResolutionKind::Invalid,
                intent,
                Some("Authority degraded: invalid registry".to_owned()),
            );
        }
    };
    let registry_text = match String::from_utf8(registry_bytes) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("[memory authority] registry is not UTF-8: {error}");
            return Resolution::simple(
                ResolutionKind::Invalid,
                intent,
                Some("Authority degraded: invalid registry".to_owned()),
            );
        }
    };
    let registry: Registry = match toml::from_str(&registry_text) {
        Ok(registry) => registry,
        Err(error) => {
            eprintln!("[memory authority] registry parse failed: {error}");
            return Resolution::simple(
                ResolutionKind::Invalid,
                intent,
                Some("Authority degraded: invalid registry".to_owned()),
            );
        }
    };
    if registry.version != 1 {
        return Resolution::simple(
            ResolutionKind::Unsupported,
            intent,
            Some("Authority degraded: unsupported registry version".to_owned()),
        );
    }
    if let Err(detail) = validate_registry(&registry) {
        let notice = if detail == "registry entry limit" {
            "Authority degraded: registry entry limit"
        } else {
            "Authority degraded: invalid registry"
        };
        eprintln!("[memory authority] {detail}");
        return Resolution::simple(ResolutionKind::Invalid, intent, Some(notice.to_owned()));
    }

    let query_tokens = tokens(query);
    let matched: Vec<RegistrySource> = registry
        .sources
        .iter()
        .filter(|source| {
            source.topics.iter().any(|topic| {
                topic_tokens(topic).is_some_and(|required| {
                    required.iter().all(|token| query_tokens.contains(token))
                })
            })
        })
        .cloned()
        .collect();
    if matched.is_empty() {
        return Resolution::simple(ResolutionKind::Unmatched, intent, None);
    }
    if matched.len() > MAX_MATCHES {
        return Resolution::simple(
            ResolutionKind::Conflict,
            intent,
            Some("Authority conflict: matching source limit exceeded".to_owned()),
        );
    }
    let matched_count = matched.len();
    let mut eligible = match effective_sources(&registry, matched, for_agent) {
        Ok(eligible) => eligible,
        Err(detail) => {
            eprintln!("[memory authority] {detail}");
            return Resolution::simple(
                ResolutionKind::Invalid,
                intent,
                Some(format!("Authority degraded: {detail}")),
            );
        }
    };
    if eligible.is_empty() && matched_count > 0 {
        return Resolution::simple(
            ResolutionKind::PrivateOnly,
            intent,
            Some("Authority source excluded: private".to_owned()),
        );
    }

    let current_ids: Vec<String> = eligible
        .iter()
        .filter(|entry| entry.status == AuthorityStatus::Current)
        .map(|entry| entry.source.id.clone())
        .collect();
    if current_ids.len() > 1 {
        return Resolution::simple(
            ResolutionKind::Conflict,
            intent,
            Some(format!("Authority conflict: {}", current_ids.join(", "))),
        );
    }

    eligible.sort_by_key(|entry| match (intent, entry.status) {
        (QueryIntent::Current, AuthorityStatus::Current)
        | (QueryIntent::Historical, AuthorityStatus::Historical) => 0,
        (QueryIntent::Current, AuthorityStatus::Unknown)
        | (QueryIntent::Historical, AuthorityStatus::Current) => 1,
        _ => 2,
    });
    if intent == QueryIntent::Current && !current_ids.is_empty() {
        eligible.retain(|entry| entry.status == AuthorityStatus::Current);
    } else if intent == QueryIntent::Historical
        && eligible
            .iter()
            .any(|entry| entry.status == AuthorityStatus::Historical)
    {
        eligible.retain(|entry| entry.status == AuthorityStatus::Historical);
    }
    eligible.truncate(MAX_SOURCES);

    let mut total_bytes = 0usize;
    let mut sources = Vec::new();
    for entry in eligible {
        let bytes = match read_root_relative(
            hex_root,
            Path::new(&entry.source.path),
            SOURCE_LIMIT,
            ReadPurpose::SourceBody,
        ) {
            Ok(bytes) => bytes,
            Err(error) => {
                eprintln!(
                    "[memory authority] source {} read failed: {error}",
                    entry.source.id
                );
                return Resolution::simple(
                    ResolutionKind::Invalid,
                    intent,
                    Some(format!(
                        "Authority degraded: source {} unavailable",
                        entry.source.id
                    )),
                );
            }
        };
        total_bytes = total_bytes.saturating_add(bytes.len());
        if total_bytes > SOURCE_AGGREGATE_LIMIT {
            return Resolution::simple(
                ResolutionKind::Invalid,
                intent,
                Some("Authority degraded: aggregate source limit".to_owned()),
            );
        }
        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(error) => {
                eprintln!(
                    "[memory authority] source {} is not UTF-8: {error}",
                    entry.source.id
                );
                return Resolution::simple(
                    ResolutionKind::Invalid,
                    intent,
                    Some(format!(
                        "Authority degraded: source {} is invalid",
                        entry.source.id
                    )),
                );
            }
        };
        let excerpt = match extract_excerpt(&text, entry.source.heading.as_deref()) {
            Some(excerpt) if !excerpt.is_empty() => excerpt,
            _ => {
                return Resolution::simple(
                    ResolutionKind::Invalid,
                    intent,
                    Some(format!(
                        "Authority degraded: source {} heading unavailable",
                        entry.source.id
                    )),
                );
            }
        };
        sources.push(ResolvedSource {
            id: entry.source.id,
            path: entry.source.path,
            status: entry.status,
            effective_date: entry.source.effective_date,
            superseded_by: entry.source.superseded_by,
            excerpt,
        });
    }

    Resolution {
        kind: ResolutionKind::Matched,
        intent,
        sources,
        notice: None,
    }
}

fn markdown_heading(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    let level = trimmed.bytes().take_while(|byte| *byte == b'#').count();
    if level == 0 || level > 6 || trimmed.as_bytes().get(level) != Some(&b' ') {
        return None;
    }
    Some((level, trimmed.get(level + 1..)?.trim()))
}

fn extract_excerpt(text: &str, heading: Option<&str>) -> Option<String> {
    let selected = match heading {
        None => text.trim().to_owned(),
        Some(wanted) => {
            let lines: Vec<&str> = text.lines().collect();
            let (start, level) = lines.iter().enumerate().find_map(|(index, line)| {
                markdown_heading(line)
                    .filter(|(_, title)| *title == wanted)
                    .map(|(level, _)| (index + 1, level))
            })?;
            let end = lines[start..]
                .iter()
                .position(|line| markdown_heading(line).is_some_and(|(next, _)| next <= level))
                .map_or(lines.len(), |offset| start + offset);
            lines[start..end].join("\n").trim().to_owned()
        }
    };
    let excerpt: String = selected.chars().take(MAX_EXCERPT_CHARS).collect();
    Some(excerpt.trim().to_owned())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReadPurpose {
    Registry,
    SourceBody,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

#[cfg(unix)]
fn identity(metadata: &fs::Metadata) -> FileIdentity {
    use std::os::unix::fs::MetadataExt;
    FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

fn read_root_relative(
    root: &Path,
    relative: &Path,
    limit: u64,
    purpose: ReadPurpose,
) -> io::Result<Vec<u8>> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    let relative_text = relative
        .to_str()
        .ok_or_else(|| io::Error::other("authority path is not UTF-8"))?;
    if !valid_relative_path(relative_text) {
        return Err(io::Error::other(
            "authority path is not a bounded relative path",
        ));
    }
    let root_metadata = fs::symlink_metadata(root)?;
    if !root_metadata.file_type().is_dir() || root_metadata.file_type().is_symlink() {
        return Err(io::Error::other("authority root is not a real directory"));
    }
    let mut ancestors = vec![(root.to_path_buf(), identity(&root_metadata))];
    let mut cursor = root.to_path_buf();
    let components: Vec<_> = relative.components().collect();
    for component in &components[..components.len() - 1] {
        cursor.push(component.as_os_str());
        let metadata = fs::symlink_metadata(&cursor)?;
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            return Err(io::Error::other(
                "authority path ancestry contains an alias",
            ));
        }
        ancestors.push((cursor.clone(), identity(&metadata)));
    }
    let path = root.join(relative);
    let before = fs::symlink_metadata(&path)?;
    if !before.file_type().is_file() || before.file_type().is_symlink() || before.nlink() != 1 {
        return Err(io::Error::other(
            "authority source is not an owned regular file",
        ));
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&path)?;
    let opened = file.metadata()?;
    if !opened.is_file() || opened.nlink() != 1 || identity(&opened) != identity(&before) {
        return Err(io::Error::other("authority source changed during open"));
    }
    observe_open(&path, purpose);
    let mut bytes = Vec::new();
    (&file).take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::other("authority file exceeds its byte limit"));
    }
    let after = fs::symlink_metadata(&path)?;
    if !after.file_type().is_file()
        || after.file_type().is_symlink()
        || after.nlink() != 1
        || identity(&after) != identity(&opened)
    {
        return Err(io::Error::other(
            "authority source path changed during read",
        ));
    }
    for (ancestor, expected) in ancestors {
        let metadata = fs::symlink_metadata(&ancestor)?;
        if !metadata.file_type().is_dir()
            || metadata.file_type().is_symlink()
            || identity(&metadata) != expected
        {
            return Err(io::Error::other(
                "authority source ancestry changed during read",
            ));
        }
    }
    Ok(bytes)
}

#[cfg(test)]
#[derive(Default)]
struct OpenObserver {
    watched_root: Option<PathBuf>,
    opened_sources: Vec<PathBuf>,
    replace_path: Option<PathBuf>,
    replacement: Option<Vec<u8>>,
}

#[cfg(test)]
static OPEN_OBSERVER: std::sync::OnceLock<std::sync::Mutex<OpenObserver>> =
    std::sync::OnceLock::new();

#[cfg(test)]
static OPEN_OBSERVER_SERIAL: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();

#[cfg(test)]
fn observe_open(path: &Path, purpose: ReadPurpose) {
    if purpose != ReadPurpose::SourceBody {
        return;
    }
    let observer = OPEN_OBSERVER.get_or_init(|| std::sync::Mutex::new(OpenObserver::default()));
    let mut observer = observer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !observer
        .watched_root
        .as_deref()
        .is_some_and(|root| path.starts_with(root))
    {
        return;
    }
    observer.opened_sources.push(path.to_path_buf());
    if observer.replace_path.as_deref() == Some(path) {
        let replacement = observer.replacement.take();
        observer.replace_path = None;
        let Some(replacement) = replacement else {
            return;
        };
        fs::remove_file(path).expect("replace observed authority source: unlink old path");
        fs::write(path, replacement).expect("replace observed authority source: write new path");
    }
}

#[cfg(not(test))]
fn observe_open(_path: &Path, _purpose: ReadPurpose) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn write(root: &Path, relative: &str, body: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn registry(entries: &str) -> String {
        format!("version = 1\n{entries}")
    }

    fn watch(root: &Path, replace_path: Option<PathBuf>, replacement: Option<&[u8]>) {
        let observer = OPEN_OBSERVER.get_or_init(|| std::sync::Mutex::new(OpenObserver::default()));
        *observer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = OpenObserver {
            watched_root: Some(root.to_path_buf()),
            opened_sources: Vec::new(),
            replace_path,
            replacement: replacement.map(<[u8]>::to_vec),
        };
    }

    fn opened_sources() -> Vec<PathBuf> {
        OPEN_OBSERVER
            .get_or_init(|| std::sync::Mutex::new(OpenObserver::default()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .opened_sources
            .clone()
    }

    #[test]
    fn reader_rejects_final_ancestor_hardlink_and_replacement_aliases() {
        let _serial = OPEN_OBSERVER_SERIAL
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let final_link = tempfile::TempDir::new().unwrap();
        write(final_link.path(), "real.md", "real");
        fs::create_dir_all(final_link.path().join("docs")).unwrap();
        symlink("../real.md", final_link.path().join("docs/source.md")).unwrap();
        assert!(read_root_relative(
            final_link.path(),
            Path::new("docs/source.md"),
            128,
            ReadPurpose::SourceBody
        )
        .is_err());

        let root_link = tempfile::TempDir::new().unwrap();
        fs::create_dir_all(root_link.path().join("real/docs")).unwrap();
        write(root_link.path(), "real/docs/source.md", "real");
        symlink("real", root_link.path().join("alias-root")).unwrap();
        assert!(read_root_relative(
            &root_link.path().join("alias-root"),
            Path::new("docs/source.md"),
            128,
            ReadPurpose::SourceBody
        )
        .is_err());

        let ancestor_link = tempfile::TempDir::new().unwrap();
        fs::create_dir_all(ancestor_link.path().join("real")).unwrap();
        write(ancestor_link.path(), "real/source.md", "real");
        symlink("real", ancestor_link.path().join("docs")).unwrap();
        assert!(read_root_relative(
            ancestor_link.path(),
            Path::new("docs/source.md"),
            128,
            ReadPurpose::SourceBody
        )
        .is_err());

        let hard_link = tempfile::TempDir::new().unwrap();
        write(hard_link.path(), "docs/source.md", "real");
        fs::hard_link(
            hard_link.path().join("docs/source.md"),
            hard_link.path().join("docs/alias.md"),
        )
        .unwrap();
        assert!(read_root_relative(
            hard_link.path(),
            Path::new("docs/source.md"),
            128,
            ReadPurpose::SourceBody
        )
        .is_err());

        let replaced = tempfile::TempDir::new().unwrap();
        write(replaced.path(), "docs/source.md", "before");
        watch(
            replaced.path(),
            Some(replaced.path().join("docs/source.md")),
            Some(b"after"),
        );
        let error = read_root_relative(
            replaced.path(),
            Path::new("docs/source.md"),
            128,
            ReadPurpose::SourceBody,
        )
        .unwrap_err();
        assert!(error.to_string().contains("changed during read"));
        assert_eq!(
            opened_sources(),
            vec![replaced.path().join("docs/source.md")]
        );
    }

    #[test]
    fn metadata_conflicts_and_limits_open_no_source_bodies() {
        let _serial = OPEN_OBSERVER_SERIAL
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let conflict = tempfile::TempDir::new().unwrap();
        write(conflict.path(), "docs/a.md", "A");
        write(conflict.path(), "docs/b.md", "B");
        write(
            conflict.path(),
            REGISTRY_RELATIVE,
            &registry(
                "[[sources]]\nid='a'\npath='docs/a.md'\ntopics=['memory recall']\nauthority_status='current'\nprivate=false\n\n[[sources]]\nid='b'\npath='docs/b.md'\ntopics=['memory recall']\nauthority_status='current'\nprivate=false\n",
            ),
        );
        watch(conflict.path(), None, None);
        let result = resolve(conflict.path(), "memory recall procedure", false);
        assert_eq!(result.kind, ResolutionKind::Conflict);
        assert!(opened_sources().is_empty());

        let over_limit = tempfile::TempDir::new().unwrap();
        let mut entries = String::new();
        for index in 0..=MAX_ENTRIES {
            write(
                over_limit.path(),
                &format!("docs/s{index}.md"),
                &format!("BODY_{index}"),
            );
            entries.push_str(&format!(
                "[[sources]]\nid='s{index}'\npath='docs/s{index}.md'\ntopics=['memory recall']\nauthority_status='current'\nprivate=false\n"
            ));
        }
        write(over_limit.path(), REGISTRY_RELATIVE, &registry(&entries));
        watch(over_limit.path(), None, None);
        let result = resolve(over_limit.path(), "memory recall procedure", false);
        assert_eq!(result.kind, ResolutionKind::Invalid);
        assert!(opened_sources().is_empty());
    }
}
