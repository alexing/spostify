//! Briefings and credits on disk.
//!
//! A briefing costs a paid request and some waiting, and what it says about
//! a record changes rarely, so a cached one is shown until the listener asks
//! for a new one. There is one file per item, language and [`Mode`].
//! MusicBrainz credits are kept per item for a while too, so a regenerated
//! or deeper briefing does not wait for them again. Files are written to a
//! temporary name and renamed into place, so a crash never leaves half a
//! file behind.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

use super::musicbrainz::Credits;
use super::{Insight, Kind, Mode};

/// The format of a cached file; anything else is ignored and rewritten.
const VERSION: u32 = 1;
/// How long found credits are trusted, in seconds: MusicBrainz grows, but
/// slowly.
const CREDITS_FRESH: i64 = 30 * 24 * 60 * 60;
/// How long "no match" is trusted, in seconds: shorter, as an entry may be
/// added.
const MISS_FRESH: i64 = 7 * 24 * 60 * 60;

#[derive(Serialize, Deserialize)]
struct Cached {
    version: u32,
    insight: Insight,
}

#[derive(Serialize, Deserialize)]
struct CachedCredits {
    version: u32,
    /// When they were looked up, in seconds since the Unix epoch.
    looked_up: i64,
    /// `None` when MusicBrainz had no match.
    credits: Option<Credits>,
}

fn hashed(prefix: &str, text: &str) -> String {
    let digest = Sha1::digest(text.as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("{prefix}-{hex}.json")
}

/// The file name for an item's briefing in a language and mode. Hashing
/// keeps any id within the folder and makes each its own file. Deep
/// briefings keep the name briefings had before there were modes, so the
/// ones saved then are still found.
pub fn key(kind: Kind, id: &str, language: &str, mode: Mode) -> String {
    let mut text = format!("{}|{id}|{language}", kind.as_str());
    if mode == Mode::Quick {
        text.push_str("|quick");
    }
    hashed(kind.as_str(), &text)
}

pub fn path(dir: &Path, kind: Kind, id: &str, language: &str, mode: Mode) -> PathBuf {
    dir.join(key(kind, id, language, mode))
}

/// The file for an item's MusicBrainz credits, whatever the language.
pub fn credits_path(dir: &Path, kind: Kind, id: &str) -> PathBuf {
    dir.join(hashed(
        &format!("credits-{}", kind.as_str()),
        &format!("credits|{}|{id}", kind.as_str()),
    ))
}

/// The briefing stored at `path`, marked as cached.
pub fn read(path: &Path) -> Option<Insight> {
    let cached: Cached = read_json(path)?;
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
    write_json(
        path,
        &Cached {
            version: VERSION,
            insight: insight.clone(),
        },
    )
}

/// The credits stored at `path` if they are still fresh at `now`:
/// `Some(None)` is a saved "no match".
pub fn read_credits(path: &Path, now: i64) -> Option<Option<Credits>> {
    let cached: CachedCredits = read_json(path)?;
    let fresh = if cached.credits.is_some() {
        CREDITS_FRESH
    } else {
        MISS_FRESH
    };
    let age = now.saturating_sub(cached.looked_up);
    (cached.version == VERSION && (0..fresh).contains(&age)).then_some(cached.credits)
}

/// Stores what a lookup at `now` found, `None` meaning no match.
pub fn write_credits(path: &Path, credits: Option<&Credits>, now: i64) -> std::io::Result<()> {
    write_json(
        path,
        &CachedCredits {
            version: VERSION,
            looked_up: now,
            credits: credits.cloned(),
        },
    )
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Writes `value` beside `path` and renames it into place. The temporary
/// name is unique, so two briefings saving the same credits at once never
/// write into each other's file.
fn write_json<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("cache path has no folder"))?;
    std::fs::create_dir_all(parent)?;
    let text = serde_json::to_string_pretty(value).map_err(std::io::Error::other)?;
    let temporary = path.with_extension(format!(
        "{}.{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    if let Err(error) =
        std::fs::write(&temporary, text).and_then(|()| crate::util::replace_file(&temporary, path))
    {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(())
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
            mode: Mode::Deep,
            cached: false,
        }
    }

    #[test]
    fn keys_differ_by_kind_id_language_and_mode() {
        let base = key(Kind::Album, "abc", "es", Mode::Deep);
        assert!(base.starts_with("album-"));
        assert!(base.ends_with(".json"));
        assert_eq!(base, key(Kind::Album, "abc", "es", Mode::Deep));
        assert_ne!(base, key(Kind::Album, "abc", "en", Mode::Deep));
        assert_ne!(base, key(Kind::Album, "abd", "es", Mode::Deep));
        assert_ne!(base, key(Kind::Album, "abc", "es", Mode::Quick));
        assert_ne!(
            key(Kind::Track, "abc", "es", Mode::Deep).split('-').nth(1),
            base.split('-').nth(1)
        );
        let credits = credits_path(Path::new("x"), Kind::Album, "abc");
        let credits = credits.file_name().unwrap().to_str().unwrap();
        assert!(credits.starts_with("credits-album-"));
        assert_ne!(credits, base);
    }

    #[test]
    fn deep_briefings_keep_the_name_from_before_modes() {
        // The SHA-1 of "album|abc|es", as the first version named it.
        assert_eq!(
            key(Kind::Album, "abc", "es", Mode::Deep),
            format!("album-{}.json", {
                let digest = Sha1::digest(b"album|abc|es");
                digest
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>()
            })
        );
    }

    #[test]
    fn each_mode_has_its_own_file() {
        let dir = Scratch::new("modes");
        let deep = path(dir.path(), Kind::Album, "abc", "es", Mode::Deep);
        let quick = path(dir.path(), Kind::Album, "abc", "es", Mode::Quick);
        write(
            &quick,
            &Insight {
                markdown: "Quick".into(),
                ..insight()
            },
        )
        .unwrap();
        assert!(read(&deep).is_none());
        write(&deep, &insight()).unwrap();
        assert_eq!(read(&quick).unwrap().markdown, "Quick");
        assert_eq!(read(&deep).unwrap().markdown, insight().markdown);
    }

    #[test]
    fn credits_round_trip_and_go_stale() {
        use super::super::musicbrainz::Credit;
        let dir = Scratch::new("credits");
        let path = credits_path(&dir.path().join("insights"), Kind::Track, "t1");
        assert_eq!(read_credits(&path, 1000), None);
        let credits = Credits {
            url: "https://musicbrainz.org/recording/x".into(),
            facts: vec![("First released".into(), "1984-06-25".into())],
            credits: vec![Credit {
                role: "producer".into(),
                name: "Prince".into(),
                on: Some("Purple Rain".into()),
            }],
        };
        write_credits(&path, Some(&credits), 1000).unwrap();
        assert_eq!(read_credits(&path, 1000), Some(Some(credits.clone())));
        assert_eq!(
            read_credits(&path, 1000 + CREDITS_FRESH - 1),
            Some(Some(credits))
        );
        assert_eq!(read_credits(&path, 1000 + CREDITS_FRESH), None);
        // A clock set back does not make old credits fresh forever.
        assert_eq!(read_credits(&path, 999), None);
        // A miss is remembered, for less long.
        write_credits(&path, None, 1000).unwrap();
        assert_eq!(read_credits(&path, 1000 + MISS_FRESH - 1), Some(None));
        assert_eq!(read_credits(&path, 1000 + MISS_FRESH), None);
        let leftovers = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|ext| ext == "tmp")
            })
            .count();
        assert_eq!(leftovers, 0);
    }

    #[test]
    fn keys_stay_inside_the_folder_whatever_the_id() {
        let name = key(Kind::Track, "../../etc/passwd", "es", Mode::Quick);
        assert!(!name.contains('/'));
        assert!(!name.contains(".."));
    }

    #[test]
    fn a_written_insight_reads_back_marked_as_cached() {
        let dir = Scratch::new("roundtrip");
        let path = path(
            &dir.path().join("insights"),
            Kind::Album,
            "abc",
            "es",
            Mode::Deep,
        );
        write(&path, &insight()).unwrap();
        let read = read(&path).unwrap();
        assert!(read.cached);
        assert_eq!(read.markdown, insight().markdown);
        assert_eq!(read.created_at, insight().created_at);
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[test]
    fn rewriting_replaces_the_previous_insight() {
        let dir = Scratch::new("rewrite");
        let path = path(dir.path(), Kind::Artist, "x", "en", Mode::Quick);
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
        let path = path(dir.path(), Kind::Album, "abc", "es", Mode::Deep);
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
