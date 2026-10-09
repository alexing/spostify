//! AI insights: a short, grounded briefing about an album, an artist or a
//! song, written by Claude.
//!
//! The flow runs on the backend runtime, never on the UI thread:
//!
//! 1. The interface describes the item with what Spotify already told it
//!    (a [`Subject`]).
//! 2. A cached briefing for the same item, language and [`Mode`] is
//!    returned at once unless the listener asked to regenerate it
//!    ([`cache`]). Opening an item shows its deep briefing when one is
//!    saved, else its quick one.
//! 3. MusicBrainz is asked for credits: producers, engineers, players,
//!    studios and writers ([`musicbrainz`]). Credits are saved too, so a
//!    regenerated or deeper briefing skips this step. A miss or a failure
//!    there only means the briefing has less to stand on.
//! 4. Claude writes the briefing in the interface's language ([`claude`]),
//!    streamed to the panel as it is written. A quick briefing comes from
//!    the credits and Claude's own knowledge; a deep one adds web search.
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
pub mod sse;
pub mod stream;

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

/// How much work goes into a briefing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// A short briefing from the credits and Claude's own knowledge, with
    /// little thinking and no tools: it starts appearing within seconds.
    #[default]
    Quick,
    /// A longer briefing that searches the web to confirm and complete it.
    Deep,
}

/// What a briefing being written is doing, for the panel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    /// Looking up credits on MusicBrainz.
    Credits,
    /// Claude is writing.
    #[default]
    Writing,
    /// Claude is searching the web.
    Searching,
}

/// News about a briefing being written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Progress {
    Phase(Phase),
    /// More of the answer, to append to what came before.
    Text(String),
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
    pub mode: Mode,
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
    /// Which kind of briefing it is; the cache file it lives in says so.
    #[serde(skip)]
    pub mode: Mode,
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
    /// A briefing being written has news; several arrive before it ends.
    Progress {
        id: u64,
        progress: Progress,
    },
    Generated {
        id: u64,
        result: Result<Insight, Error>,
    },
    Key(KeyStatus),
}

/// Runs one job to completion on the backend runtime. `report` hands
/// progress to the interface while the job runs; the returned outcome is
/// the last word.
pub async fn run(
    job: Job,
    http: Result<reqwest::Client, String>,
    cache_dir: PathBuf,
    report: &(dyn Fn(Outcome) + Send + Sync),
) -> Outcome {
    match job {
        Job::Generate(request) => {
            let id = request.id;
            let mut progress = |progress| report(Outcome::Progress { id, progress });
            Outcome::Generated {
                id,
                result: generate(&request, http, &cache_dir, &mut progress).await,
            }
        }
        Job::CheckKey => Outcome::Key(key::status().await),
        Job::StoreKey(secret) => Outcome::Key(key::store(secret).await),
        Job::ForgetKey => Outcome::Key(key::forget().await),
    }
}

async fn generate(
    request: &Request,
    http: Result<reqwest::Client, String>,
    cache_dir: &std::path::Path,
    progress: &mut (dyn FnMut(Progress) + Send),
) -> Result<Insight, Error> {
    let subject = &request.subject;
    let path = |mode| {
        cache::path(
            cache_dir,
            subject.kind,
            &subject.id,
            &request.language_tag,
            mode,
        )
    };
    if !request.force {
        // A quick look shows the deep briefing when there is one.
        let saved: &[Mode] = match request.mode {
            Mode::Quick => &[Mode::Deep, Mode::Quick],
            Mode::Deep => &[Mode::Deep],
        };
        for &mode in saved {
            if let Some(cached) = cache::read(&path(mode)) {
                return Ok(Insight { mode, ..cached });
            }
        }
    }
    let api_key = key::resolve().await.ok_or(Error::MissingKey)?;
    let http = http.map_err(Error::Network)?;
    let credits = credits(&http, subject, cache_dir, progress).await;
    progress(Progress::Phase(Phase::Writing));
    let user = prompt::user_message(
        subject,
        credits.as_ref(),
        request.mode,
        &request.language_name,
        &request.language_tag,
    );
    let answer = claude::generate(&http, &api_key, request.mode, &user, progress).await?;
    let insight = Insight {
        markdown: answer.text,
        created_at: jiff::Timestamp::now().as_second(),
        truncated: answer.truncated,
        mode: request.mode,
        cached: false,
    };
    if let Err(error) = cache::write(&path(request.mode), &insight) {
        log::warn!("unable to cache the insight: {error}");
    }
    Ok(insight)
}

/// The longest wait for MusicBrainz before writing without credits.
const CREDITS_DEADLINE: std::time::Duration = std::time::Duration::from_secs(25);

/// The item's MusicBrainz credits, from the disk when they were looked up
/// recently. A failed lookup is not saved, so the next briefing asks again.
async fn credits(
    http: &reqwest::Client,
    subject: &Subject,
    cache_dir: &std::path::Path,
    progress: &mut (dyn FnMut(Progress) + Send),
) -> Option<musicbrainz::Credits> {
    let path = cache::credits_path(cache_dir, subject.kind, &subject.id);
    let now = jiff::Timestamp::now().as_second();
    if let Some(saved) = cache::read_credits(&path, now) {
        return saved;
    }
    progress(Progress::Phase(Phase::Credits));
    match tokio::time::timeout(CREDITS_DEADLINE, musicbrainz::lookup(http, subject)).await {
        Ok(Ok(credits)) => {
            if let Err(error) = cache::write_credits(&path, credits.as_ref(), now) {
                log::warn!("unable to cache MusicBrainz credits: {error}");
            }
            credits
        }
        Ok(Err(error)) => {
            log::info!("MusicBrainz lookup failed, continuing without credits: {error:#}");
            None
        }
        Err(_) => {
            log::info!("MusicBrainz is slow, continuing without credits");
            None
        }
    }
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
    /// Write a fresh briefing of the kind the panel shows.
    Regenerate,
    /// Show the deep briefing for the open item, writing it if needed.
    Deepen,
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
    /// Being written: what it is doing, and the text so far.
    Loading {
        phase: Phase,
        text: String,
    },
    Ready(Insight),
    Failed(Error),
}

impl Status {
    fn loading(phase: Phase, text: &str) -> Self {
        Self::Loading {
            phase,
            text: text.to_string(),
        }
    }
}

/// The open panel.
#[derive(Clone, Debug)]
pub struct Panel {
    pub request: u64,
    pub subject: Subject,
    /// The kind of briefing shown or being written.
    pub mode: Mode,
    pub status: Status,
}

/// A briefing being written.
struct InFlight {
    kind: Kind,
    id: String,
    language: String,
    mode: Mode,
    request: u64,
    phase: Phase,
    /// What has arrived so far, so a panel that comes back to it shows it.
    text: String,
}

/// The feature's state on the interface thread.
#[derive(Default)]
pub struct State {
    pub panel: Option<Panel>,
    pub key: KeyStatus,
    /// The key being typed in Settings; cleared once handed to the store.
    pub key_draft: String,
    next_request: u64,
    /// Briefings being written. Opening the same item again joins its
    /// request instead of paying for another.
    in_flight: Vec<InFlight>,
    /// The demo's sample, being "streamed".
    #[cfg(any(test, feature = "demo"))]
    demo: Option<demo::Stream>,
}

impl State {
    /// Shows `subject` as loading and returns the request to send for it,
    /// or `None` when the same briefing is already being written.
    pub fn begin(
        &mut self,
        subject: Subject,
        locale: crate::i18n::Locale,
        mode: Mode,
        force: bool,
    ) -> Option<Request> {
        let tag = locale.tag();
        if !force {
            // A quick look joins a deep briefing being written, which is
            // what it would show once saved.
            let joinable: &[Mode] = match mode {
                Mode::Quick => &[Mode::Deep, Mode::Quick],
                Mode::Deep => &[Mode::Deep],
            };
            let pending = joinable.iter().find_map(|&wanted| {
                self.in_flight.iter().find(|entry| {
                    entry.kind == subject.kind
                        && entry.id == subject.id
                        && entry.language == tag
                        && entry.mode == wanted
                })
            });
            if let Some(entry) = pending {
                self.panel = Some(Panel {
                    request: entry.request,
                    subject,
                    mode: entry.mode,
                    status: Status::loading(entry.phase, &entry.text),
                });
                return None;
            }
        }
        self.next_request += 1;
        let id = self.next_request;
        self.in_flight.push(InFlight {
            kind: subject.kind,
            id: subject.id.clone(),
            language: tag.to_string(),
            mode,
            request: id,
            phase: Phase::default(),
            text: String::new(),
        });
        self.panel = Some(Panel {
            request: id,
            subject: subject.clone(),
            mode,
            status: Status::loading(Phase::default(), ""),
        });
        Some(Request {
            id,
            subject,
            language_tag: tag.to_string(),
            language_name: language_name(locale),
            mode,
            force,
        })
    }

    /// Applies news about a briefing being written. The panel follows it
    /// only while it shows that briefing.
    pub fn progress(&mut self, id: u64, progress: Progress) {
        let Some(entry) = self.in_flight.iter_mut().find(|entry| entry.request == id) else {
            return;
        };
        match progress {
            Progress::Phase(phase) => entry.phase = phase,
            Progress::Text(text) => entry.text.push_str(&text),
        }
        if let Some(panel) = self.panel.as_mut().filter(|panel| panel.request == id) {
            panel.status = Status::loading(entry.phase, &entry.text);
        }
    }

    /// Applies an answer, unless the panel has moved on since.
    pub fn finish(&mut self, id: u64, result: Result<Insight, Error>) {
        self.in_flight.retain(|entry| entry.request != id);
        if let Some(panel) = self.panel.as_mut().filter(|panel| panel.request == id) {
            panel.status = match result {
                Ok(insight) => {
                    // A quick look may have found a saved deep briefing.
                    panel.mode = insight.mode;
                    Status::Ready(insight)
                }
                Err(error) => Status::Failed(error),
            };
        }
    }

    /// Plays `insight` into the panel a little at a time, as if Claude were
    /// writing it, for the demo. A sample still playing is finished first,
    /// so a panel that comes back to it is not left waiting.
    #[cfg(any(test, feature = "demo"))]
    pub fn play_demo(&mut self, id: u64, insight: Insight, now: std::time::Instant) {
        if let Some(previous) = self.demo.take() {
            let (id, insight) = previous.into_result();
            self.finish(id, Ok(insight));
        }
        self.demo = Some(demo::Stream::new(id, insight, now));
    }

    /// Moves the demo's sample along. Returns whether it is still playing.
    #[cfg(any(test, feature = "demo"))]
    pub fn tick_demo(&mut self, now: std::time::Instant) -> bool {
        let Some(stream) = self.demo.as_mut() else {
            return false;
        };
        let (id, due) = stream.due(now);
        for progress in due {
            self.progress(id, progress);
        }
        if self.demo.as_ref().is_some_and(demo::Stream::is_done) {
            let (id, insight) = self.demo.take().expect("checked above").into_result();
            self.finish(id, Ok(insight));
            return false;
        }
        true
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
        Locale::Ukrainian => "Ukrainian",
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
        let first = state
            .begin(
                subject.clone(),
                crate::i18n::Locale::Spanish,
                Mode::Quick,
                false,
            )
            .unwrap();
        let second = state
            .begin(subject, crate::i18n::Locale::Spanish, Mode::Quick, true)
            .unwrap();
        assert!(second.force);
        assert_eq!(second.language_tag, "es");
        state.finish(first.id, Err(Error::Empty));
        assert!(matches!(
            state.panel.as_ref().unwrap().status,
            Status::Loading { .. }
        ));
        state.finish(second.id, Err(Error::Refused));
        assert!(matches!(
            state.panel.as_ref().unwrap().status,
            Status::Failed(Error::Refused)
        ));
    }

    #[test]
    fn reopening_an_item_being_written_joins_its_request() {
        let mut state = State::default();
        let album = Subject::from_uri("spotify:album:a", "A").unwrap();
        let other = Subject::from_uri("spotify:album:b", "B").unwrap();
        let spanish = crate::i18n::Locale::Spanish;
        let first = state
            .begin(album.clone(), spanish, Mode::Quick, false)
            .unwrap();
        // The listener looks at another album, then comes back.
        let second = state.begin(other, spanish, Mode::Quick, false).unwrap();
        assert!(
            state
                .begin(album.clone(), spanish, Mode::Quick, false)
                .is_none()
        );
        assert_eq!(state.panel.as_ref().unwrap().request, first.id);
        // Another language is another briefing.
        assert!(
            state
                .begin(
                    album.clone(),
                    crate::i18n::Locale::French,
                    Mode::Quick,
                    false
                )
                .is_some()
        );
        state.begin(album.clone(), spanish, Mode::Quick, false);
        state.finish(second.id, Err(Error::Empty));
        state.finish(first.id, Err(Error::Refused));
        assert!(matches!(
            state.panel.as_ref().unwrap().status,
            Status::Failed(Error::Refused)
        ));
        // Once answered, opening it asks again (and reads the cache).
        assert!(state.begin(album, spanish, Mode::Quick, false).is_some());
    }

    fn ready(mode: Mode) -> Result<Insight, Error> {
        Ok(Insight {
            markdown: "Done".into(),
            created_at: 0,
            truncated: false,
            mode,
            cached: false,
        })
    }

    #[test]
    fn streamed_text_fills_the_panel_and_survives_a_return_visit() {
        let mut state = State::default();
        let spanish = crate::i18n::Locale::Spanish;
        let album = Subject::from_uri("spotify:album:a", "A").unwrap();
        let other = Subject::from_uri("spotify:album:b", "B").unwrap();
        let first = state
            .begin(album.clone(), spanish, Mode::Quick, false)
            .unwrap();
        state.progress(first.id, Progress::Phase(Phase::Credits));
        assert!(matches!(
            &state.panel.as_ref().unwrap().status,
            Status::Loading { phase: Phase::Credits, text } if text.is_empty()
        ));
        state.progress(first.id, Progress::Phase(Phase::Writing));
        state.progress(first.id, Progress::Text("Sign o' ".into()));
        state.progress(first.id, Progress::Text("the Times".into()));
        assert!(matches!(
            &state.panel.as_ref().unwrap().status,
            Status::Loading { phase: Phase::Writing, text } if text == "Sign o' the Times"
        ));
        // Another item's panel never shows this text.
        let second = state.begin(other, spanish, Mode::Quick, false).unwrap();
        state.progress(first.id, Progress::Text(" is".into()));
        assert_eq!(state.panel.as_ref().unwrap().request, second.id);
        assert!(matches!(
            &state.panel.as_ref().unwrap().status,
            Status::Loading { text, .. } if text.is_empty()
        ));
        // Coming back joins the request and shows everything so far.
        assert!(state.begin(album, spanish, Mode::Quick, false).is_none());
        assert!(matches!(
            &state.panel.as_ref().unwrap().status,
            Status::Loading { text, .. } if text == "Sign o' the Times is"
        ));
        state.finish(first.id, ready(Mode::Quick));
        assert!(matches!(
            state.panel.as_ref().unwrap().status,
            Status::Ready(_)
        ));
        // News for a finished request changes nothing.
        state.progress(first.id, Progress::Text("late".into()));
        assert!(matches!(
            state.panel.as_ref().unwrap().status,
            Status::Ready(_)
        ));
    }

    #[test]
    fn requests_join_by_mode() {
        let mut state = State::default();
        let spanish = crate::i18n::Locale::Spanish;
        let album = Subject::from_uri("spotify:album:a", "A").unwrap();
        let quick = state
            .begin(album.clone(), spanish, Mode::Quick, false)
            .unwrap();
        assert_eq!(quick.mode, Mode::Quick);
        // Digging deeper is another briefing, not the quick one.
        let deep = state
            .begin(album.clone(), spanish, Mode::Deep, false)
            .unwrap();
        assert_eq!(deep.mode, Mode::Deep);
        assert!(
            state
                .begin(album.clone(), spanish, Mode::Deep, false)
                .is_none()
        );
        assert_eq!(state.panel.as_ref().unwrap().request, deep.id);
        // A quick look prefers the deep briefing being written.
        assert!(
            state
                .begin(album.clone(), spanish, Mode::Quick, false)
                .is_none()
        );
        let panel = state.panel.as_ref().unwrap();
        assert_eq!((panel.request, panel.mode), (deep.id, Mode::Deep));
        // Regenerating never joins.
        let again = state
            .begin(album.clone(), spanish, Mode::Deep, true)
            .unwrap();
        assert_ne!(again.id, deep.id);
        assert!(again.force);
        state.finish(deep.id, ready(Mode::Deep));
        state.finish(again.id, ready(Mode::Deep));
        // With the deep ones done, a quick look joins the quick one.
        assert!(state.begin(album, spanish, Mode::Quick, false).is_none());
        assert_eq!(state.panel.as_ref().unwrap().request, quick.id);
    }

    #[test]
    fn a_quick_look_that_finds_a_deep_briefing_shows_it_as_deep() {
        let mut state = State::default();
        let album = Subject::from_uri("spotify:album:a", "A").unwrap();
        let request = state
            .begin(album, crate::i18n::Locale::English, Mode::Quick, false)
            .unwrap();
        assert_eq!(state.panel.as_ref().unwrap().mode, Mode::Quick);
        state.finish(request.id, ready(Mode::Deep));
        assert_eq!(state.panel.as_ref().unwrap().mode, Mode::Deep);
    }

    #[test]
    fn the_demo_sample_plays_in_pieces_then_finishes() {
        let mut state = State::default();
        let album = Subject::from_uri("spotify:album:a", "Fragments").unwrap();
        let start = std::time::Instant::now();
        for mode in [Mode::Quick, Mode::Deep] {
            let request = state
                .begin(album.clone(), crate::i18n::Locale::English, mode, true)
                .unwrap();
            let sample = demo::insight(&album, mode);
            state.play_demo(request.id, sample.clone(), start);
            let mut later = start;
            let mut pieces = 0;
            while state.tick_demo(later) {
                if let Status::Loading { text, .. } = &state.panel.as_ref().unwrap().status {
                    assert!(sample.markdown.starts_with(text.as_str()));
                    pieces += 1;
                }
                later += std::time::Duration::from_millis(40);
            }
            assert!(pieces > 5, "the sample arrives in pieces");
            let panel = state.panel.as_ref().unwrap();
            assert_eq!(panel.mode, mode);
            assert!(
                matches!(&panel.status, Status::Ready(done) if done.markdown == sample.markdown)
            );
        }
    }

    #[test]
    fn a_new_demo_sample_finishes_the_one_still_playing() {
        let mut state = State::default();
        let a = Subject::from_uri("spotify:album:a", "A").unwrap();
        let b = Subject::from_uri("spotify:album:b", "B").unwrap();
        let now = std::time::Instant::now();
        let english = crate::i18n::Locale::English;
        let first = state.begin(a.clone(), english, Mode::Quick, false).unwrap();
        state.play_demo(first.id, demo::insight(&a, Mode::Quick), now);
        let second = state.begin(b, english, Mode::Quick, false).unwrap();
        state.play_demo(second.id, demo::insight(&a, Mode::Quick), now);
        // The first is no longer in flight, so a return visit asks again.
        assert!(state.begin(a, english, Mode::Quick, false).is_some());
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
