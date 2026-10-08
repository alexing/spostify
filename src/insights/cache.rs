//! Briefings on disk, one file per item and language.
//!
//! A briefing costs a paid request and a minute of waiting, and what it says
//! about a record changes rarely, so a cached one is shown until the listener
//! asks for a new one. Files are written to a temporary name and renamed
//! into place, so a crash never leaves half a briefing behind.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

use super::{Insight, Kind};

/// The format of a cached file; anything else is ignored and rewritten.
const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct Cached {
    version: u32,
    insight: Insight,
}

/// The file name for an item in a language. Hashing keeps any id within the
/// folder and makes each (kind, id, language) its own file.
pub fn key(kind: Kind, id: &str, language: &str) -> String {
    let digest = Sha1::digest(format!("{}|{id}|{language}", kind.as_str()).as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{}-{hex}.json", kind.as_str())
}

pub fn path(dir: &Path, kind: Kind, id: &str, language: &str) -> PathBuf {
    dir.join(key(kind, id, language))
}

/// The briefing stored at `path`, marked as cached.
pub fn read(path: &Path) -> Option<Insight> {
    let text = std::fs::read_to_string(path).ok()?;
    let cached: Cached = serde_json::from_str(&text).ok()?;
    if cached.version != VERSION || cached.insight.markdown.trim().is_empty() {
        return None;
    }
    Some(Insight {
        cached: true,
        ..cached.insight
    })
}

/// Stores `insight` at `path`, replacing any earlier one atomically.
pub fn write(path: &Path, insight: &Insight) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("cache path has no folder"))?;
    std::fs::create_dir_all(parent)?;
    let text = serde_json::to_string_pretty(&Cached {
        version: VERSION,
        insight: insight.clone(),
    })
    .map_err(std::io::Error::other)?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, text)?;
    crate::util::replace_file(&temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh folder per test, removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("spotifast-insights-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            Self(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn insight() -> Insight {
        Insight {
            markdown: "## Sign o' the Times\n- Produced by Prince".into(),
            created_at: 1_790_000_000,
            truncated: false,
            cached: false,
        }
    }

    #[test]
    fn keys_differ_by_kind_id_and_language() {
        let base = key(Kind::Album, "abc", "es");
        assert!(base.starts_with("album-"));
        assert!(base.ends_with(".json"));
        assert_eq!(base, key(Kind::Album, "abc", "es"));
        assert_ne!(base, key(Kind::Album, "abc", "en"));
        assert_ne!(base, key(Kind::Album, "abd", "es"));
        assert_ne!(
            key(Kind::Track, "abc", "es").split('-').nth(1),
            base.split('-').nth(1)
        );
    }

    #[test]
    fn keys_stay_inside_the_folder_whatever_the_id() {
        let name = key(Kind::Track, "../../etc/passwd", "es");
        assert!(!name.contains('/'));
        assert!(!name.contains(".."));
    }

    #[test]
    fn a_written_insight_reads_back_marked_as_cached() {
        let dir = Scratch::new("roundtrip");
        let path = path(&dir.path().join("insights"), Kind::Album, "abc", "es");
        write(&path, &insight()).unwrap();
        let read = read(&path).unwrap();
        assert!(read.cached);
        assert_eq!(read.markdown, insight().markdown);
        assert_eq!(read.created_at, insight().created_at);
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn rewriting_replaces_the_previous_insight() {
        let dir = Scratch::new("rewrite");
        let path = path(dir.path(), Kind::Artist, "x", "en");
        write(&path, &insight()).unwrap();
        let newer = Insight {
            markdown: "Newer".into(),
            ..insight()
        };
        write(&path, &newer).unwrap();
        assert_eq!(read(&path).unwrap().markdown, "Newer");
    }

    #[test]
    fn missing_corrupt_or_empty_files_are_misses() {
        let dir = Scratch::new("miss");
        let path = path(dir.path(), Kind::Album, "abc", "es");
        assert!(read(&path).is_none());
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(&path, "{not json").unwrap();
        assert!(read(&path).is_none());
        let empty = Insight {
            markdown: "  ".into(),
            ..insight()
        };
        write(&path, &empty).unwrap();
        assert!(read(&path).is_none());
    }
}
