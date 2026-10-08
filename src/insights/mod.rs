//! AI insights: a short, grounded briefing about an album, an artist or a
//! song, written by Claude.
//!
//! The flow runs on the backend runtime, never on the UI thread:
//!
//! 1. The interface describes the item with what Spotify already told it
//!    (a [`Subject`]).
//! 2. A cached briefing for the same item and language is returned at once
//!    unless the listener asked to regenerate it ([`cache`]).
//! 3. MusicBrainz is asked for credits: producers, engineers, players,
//!    studios and writers ([`musicbrainz`]). A miss or a failure there only
//!    means the briefing has less to stand on.
//! 4. Claude writes the briefing in the interface's language, with web
//!    search for what the credits do not cover ([`claude`]).
//!
//! The Anthropic API key lives in the platform credential store ([`key`]),
//! never in `settings.json` or the log.

pub mod cache;
pub mod claude;
#[cfg(any(test, feature = "demo"))]
pub mod demo;
pub mod key;
pub mod markdown;
pub mod musicbrainz;
pub mod prompt;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::api::models::{Album, Artist, Track};

/// What kind of item a briefing is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Album,
    Artist,
    Track,
}

impl Kind {
    /// The kind a Spotify URI names, for the kinds that have briefings.
    pub fn from_uri(uri: &str) -> Option<Self> {
        match crate::util::uri_kind(uri)? {
            "album" => Some(Self::Album),
            "artist" => Some(Self::Artist),
            "track" => Some(Self::Track),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Album => "album",
            Self::Artist => "artist",
            Self::Track => "track",
        }
    }
}

/// Everything Spotify told us about the item, which grounds the briefing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subject {
    pub kind: Kind,
    /// The Spotify id, which keys the cache.
    pub id: String,
    pub name: String,
    pub artists: Vec<String>,
    /// The album a song is on.
    pub album: Option<String>,
    pub release_date: Option<String>,
    /// Spotify's `album_type` for an album: "album", "single" or
    /// "compilation". It tells an album from a single of the same name.
    pub album_type: Option<String>,
    pub label: Option<String>,
    pub genres: Vec<String>,
    /// An album's songs, or an artist's most played ones.
    pub tracks: Vec<String>,
    pub isrc: Option<String>,
    pub duration_ms: Option<u32>,
}

impl Subject {
    /// A subject known only by its URI and name; MusicBrainz and the web
    /// search fill in the rest.
    pub fn from_uri(uri: &str, name: &str) -> Option<Self> {
        Some(Self {
            kind: Kind::from_uri(uri)?,
            id: crate::util::uri_id(uri)?.to_string(),
            name: name.to_string(),
            artists: Vec::new(),
            album: None,
            release_date: None,
            album_type: None,
            label: None,
            genres: Vec::new(),
            tracks: Vec::new(),
            isrc: None,
            duration_ms: None,
        })
    }

    pub fn from_album(album: &Album, tracks: &[Track]) -> Self {
        let listed = album.tracks.as_ref().map(|page| page.items.as_slice());
        let tracks = if tracks.is_empty() {
            listed.unwrap_or_default()
        } else {
            tracks
        };
        Self {
            kind: Kind::Album,
            id: album.id.clone(),
            name: album.name.clone(),
            artists: album.artists.iter().map(|a| a.name.clone()).collect(),
            album: None,
            release_date: album.release_date.clone(),
            album_type: album.album_type.clone(),
            label: album.label.clone().filter(|label| !label.trim().is_empty()),
            genres: album.genres.clone(),
            tracks: tracks.iter().map(|track| track.name.clone()).collect(),
            isrc: None,
            duration_ms: None,
        }
    }

    pub fn from_artist(artist: &Artist, top_tracks: &[Track]) -> Self {
        Self {
            kind: Kind::Artist,
            id: artist.id.clone(),
            name: artist.name.clone(),
            artists: Vec::new(),
            album: None,
            release_date: None,
            album_type: None,
            label: None,
            genres: artist.genres.clone(),
            tracks: top_tracks.iter().map(|track| track.name.clone()).collect(),
            isrc: None,
            duration_ms: None,
        }
    }

    pub fn from_track(track: &Track) -> Option<Self> {
        Some(Self {
            kind: Kind::Track,
            id: track.id.clone().filter(|id| !id.is_empty())?,
            name: track.name.clone(),
            artists: track.artists.iter().map(|a| a.name.clone()).collect(),
            album: track.album.as_ref().map(|album| album.name.clone()),
            release_date: track
                .album
                .as_ref()
                .and_then(|album| album.release_date.clone()),
            album_type: None,
            label: track.album.as_ref().and_then(|album| album.label.clone()),
            genres: Vec::new(),
            tracks: Vec::new(),
            isrc: track.external_ids.isrc.clone(),
            duration_ms: (track.duration_ms > 0).then_some(track.duration_ms),
        })
    }

    /// The first credited artist, which most lookups search by.
    pub fn main_artist(&self) -> Option<&str> {
        self.artists.first().map(String::as_str)
    }
}

/// One briefing to write.
#[derive(Clone, Debug)]
pub struct Request {
    /// Matches the answer to the panel that asked, so a late one for an
    /// item the listener has since left is ignored.
    pub id: u64,
    pub subject: Subject,
    /// The interface language tag, such as `es`; it keys the cache.
    pub language_tag: String,
    /// The language's own name, which the prompt asks Claude to write in.
    pub language_name: String,
    /// Skip the cache and write a fresh briefing.
    pub force: bool,
}

/// A finished briefing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Insight {
    pub markdown: String,
    /// When it was written, in seconds since the Unix epoch.
    pub created_at: i64,
    /// The answer reached the token limit before it ended.
    #[serde(default)]
    pub truncated: bool,
    /// Read from the disk cache rather than written just now.
    #[serde(skip)]
    pub cached: bool,
}

/// Why no briefing could be written. Every variant is safe to show and to
/// log: none carries the key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// No key in the credential store or the environment.
    MissingKey,
    /// Anthropic rejected the key.
    InvalidKey,
    /// Still rate limited after waiting once.
    RateLimited,
    /// Anthropic stayed overloaded through the retries.
    Overloaded,
    /// Claude declined to write about this item.
    Refused,
    /// The request never reached Anthropic, or its answer never arrived.
    Network(String),
    /// Anthropic answered with an error.
    Api { status: u16, message: String },
    /// An answer without any text.
    Empty,
    /// The demo and tests have no network.
    Offline,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingKey => write!(f, "no Anthropic API key is configured"),
            Self::InvalidKey => write!(f, "the Anthropic API key was rejected"),
            Self::RateLimited => write!(f, "rate limited by Anthropic"),
            Self::Overloaded => write!(f, "Anthropic is overloaded"),
            Self::Refused => write!(f, "Claude declined the request"),
            Self::Network(detail) => write!(f, "network error: {detail}"),
            Self::Api { status, message } => write!(f, "Anthropic answered {status}: {message}"),
            Self::Empty => write!(f, "the answer had no text"),
            Self::Offline => write!(f, "offline"),
        }
    }
}

/// Where the API key comes from, as Settings shows it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeyStatus {
    /// Not asked yet.
    #[default]
    Unknown,
    /// A question to the credential store is under way.
    Checking,
    /// Saved in the credential store.
    Stored,
    /// Taken from `ANTHROPIC_API_KEY`.
    Environment,
    /// Nowhere.
    Missing,
    /// The credential store could not be reached or refused the change.
    StoreFailed,
}

/// An API key on its way to the credential store. No `Debug`, so it can
/// never be logged by accident.
#[derive(Clone)]
pub struct Secret(pub String);

/// Work for the backend.
pub enum Job {
    Generate(Box<Request>),
    CheckKey,
    StoreKey(Secret),
    ForgetKey,
}

/// What the backend reports back.
#[derive(Debug)]
pub enum Outcome {
    Generated {
        id: u64,
        result: Result<Insight, Error>,
    },
    Key(KeyStatus),
}

/// Runs one job to completion on the backend runtime.
pub async fn run(job: Job, http: Result<reqwest::Client, String>, cache_dir: PathBuf) -> Outcome {
    match job {
        Job::Generate(request) => Outcome::Generated {
            id: request.id,
            result: generate(&request, http, &cache_dir).await,
        },
        Job::CheckKey => Outcome::Key(key::status().await),
        Job::StoreKey(secret) => Outcome::Key(key::store(secret).await),
        Job::ForgetKey => Outcome::Key(key::forget().await),
    }
}

async fn generate(
    request: &Request,
    http: Result<reqwest::Client, String>,
    cache_dir: &std::path::Path,
) -> Result<Insight, Error> {
    let path = cache::path(
        cache_dir,
        request.subject.kind,
        &request.subject.id,
        &request.language_tag,
    );
    if !request.force
        && let Some(cached) = cache::read(&path)
    {
        return Ok(cached);
    }
    let api_key = key::resolve().await.ok_or(Error::MissingKey)?;
    let http = http.map_err(Error::Network)?;
    let credits = match musicbrainz::lookup(&http, &request.subject).await {
        Ok(credits) => credits,
        Err(error) => {
            log::info!("MusicBrainz lookup failed, continuing without credits: {error:#}");
            None
        }
    };
    let user = prompt::user_message(
        &request.subject,
        credits.as_ref(),
        &request.language_name,
        &request.language_tag,
    );
    let answer = claude::generate(&http, &api_key, &user).await?;
    let insight = Insight {
        markdown: answer.text,
        created_at: jiff::Timestamp::now().as_second(),
        truncated: answer.truncated,
        cached: false,
    };
    if let Err(error) = cache::write(&path, &insight) {
        log::warn!("unable to cache the insight: {error}");
    }
    Ok(insight)
}

/// The cache folder for briefings, beside the lyrics and artwork caches.
pub fn cache_dir(dirs: &crate::paths::AppDirs) -> PathBuf {
    dirs.cache.join("insights")
}

/// What the interface does with the feature.
#[derive(Clone, Debug)]
pub enum Action {
    /// Open the panel for a Spotify URI. A row's song comes along, since
    /// its details may be nowhere else in the app.
    Open {
        uri: String,
        name: String,
        track: Option<Box<Track>>,
    },
    /// Write a fresh briefing for the open item.
    Regenerate,
    Close,
    /// Ask where the key comes from, for Settings.
    CheckKey,
    /// Save the key typed in Settings.
    SaveKey,
    ForgetKey,
    /// Go to the key's row in Settings.
    OpenSettings,
}

/// The panel's content.
#[derive(Clone, Debug)]
pub enum Status {
    Loading,
    Ready(Insight),
    Failed(Error),
}

/// The open panel.
#[derive(Clone, Debug)]
pub struct Panel {
    pub request: u64,
    pub subject: Subject,
    pub status: Status,
}

/// The feature's state on the interface thread.
#[derive(Default)]
pub struct State {
    pub panel: Option<Panel>,
    pub key: KeyStatus,
    /// The key being typed in Settings; cleared once handed to the store.
    pub key_draft: String,
    next_request: u64,
}

impl State {
    /// Shows `subject` as loading and returns the request for it.
    pub fn begin(&mut self, subject: Subject, locale: crate::i18n::Locale, force: bool) -> Request {
        self.next_request += 1;
        let id = self.next_request;
        self.panel = Some(Panel {
            request: id,
            subject: subject.clone(),
            status: Status::Loading,
        });
        Request {
            id,
            subject,
            language_tag: locale.tag().to_string(),
            language_name: language_name(locale),
            force,
        }
    }

    /// Applies an answer, unless the panel has moved on since.
    pub fn finish(&mut self, id: u64, result: Result<Insight, Error>) {
        if let Some(panel) = self.panel.as_mut().filter(|panel| panel.request == id) {
            panel.status = match result {
                Ok(insight) => Status::Ready(insight),
                Err(error) => Status::Failed(error),
            };
        }
    }
}

/// The language Claude is asked to write in: its English name, which reads
/// unambiguously in the prompt, and its own name.
pub fn language_name(locale: crate::i18n::Locale) -> String {
    use crate::i18n::Locale;
    let english = match locale {
        Locale::English => "English",
        Locale::German => "German",
        Locale::Spanish => "Spanish",
        Locale::Dutch => "Dutch",
        Locale::PortugueseBrazil => "Brazilian Portuguese",
        Locale::PortuguesePortugal => "European Portuguese",
        Locale::French => "French",
        Locale::Swedish => "Swedish",
        Locale::Polish => "Polish",
        Locale::Russian => "Russian",
        Locale::Italian => "Italian",
        Locale::Japanese => "Japanese",
        Locale::ChineseSimplified => "Simplified Chinese",
        Locale::ChineseTraditional => "Traditional Chinese",
        Locale::Turkish => "Turkish",
    };
    if english == locale.native_name() {
        english.to_string()
    } else {
        format!("{english} ({})", locale.native_name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::models::{ArtistRef, ExternalIds};

    #[test]
    fn kinds_come_from_uris() {
        assert_eq!(Kind::from_uri("spotify:album:abc"), Some(Kind::Album));
        assert_eq!(Kind::from_uri("spotify:artist:abc"), Some(Kind::Artist));
        assert_eq!(Kind::from_uri("spotify:track:abc"), Some(Kind::Track));
        assert_eq!(Kind::from_uri("spotify:playlist:abc"), None);
        assert_eq!(Kind::from_uri("nonsense"), None);
    }

    #[test]
    fn a_track_subject_carries_its_album_and_isrc() {
        let track = Track {
            id: Some("t1".into()),
            name: "U Got the Look".into(),
            artists: vec![ArtistRef {
                id: Some("a1".into()),
                name: "Prince".into(),
                uri: None,
            }],
            album: Some(Album {
                name: "Sign o' the Times".into(),
                release_date: Some("1987-03-30".into()),
                ..Album::default()
            }),
            duration_ms: 227_000,
            external_ids: ExternalIds {
                isrc: Some("USWB10000001".into()),
            },
            ..Track::default()
        };
        let subject = Subject::from_track(&track).unwrap();
        assert_eq!(subject.kind, Kind::Track);
        assert_eq!(subject.album.as_deref(), Some("Sign o' the Times"));
        assert_eq!(subject.isrc.as_deref(), Some("USWB10000001"));
        assert_eq!(subject.main_artist(), Some("Prince"));
        assert_eq!(subject.duration_ms, Some(227_000));
    }

    #[test]
    fn a_local_track_without_an_id_has_no_subject() {
        assert!(Subject::from_track(&Track::default()).is_none());
    }

    #[test]
    fn late_answers_for_an_earlier_panel_are_ignored() {
        let mut state = State::default();
        let subject = Subject::from_uri("spotify:album:a", "A").unwrap();
        let first = state.begin(subject.clone(), crate::i18n::Locale::Spanish, false);
        let second = state.begin(subject, crate::i18n::Locale::Spanish, true);
        assert!(second.force);
        assert_eq!(second.language_tag, "es");
        state.finish(first.id, Err(Error::Empty));
        assert!(matches!(
            state.panel.as_ref().unwrap().status,
            Status::Loading
        ));
        state.finish(second.id, Err(Error::Refused));
        assert!(matches!(
            state.panel.as_ref().unwrap().status,
            Status::Failed(Error::Refused)
        ));
    }

    #[test]
    fn language_names_read_well_in_a_prompt() {
        assert_eq!(
            language_name(crate::i18n::Locale::Spanish),
            "Spanish (Español)"
        );
        assert_eq!(language_name(crate::i18n::Locale::English), "English");
    }
}
