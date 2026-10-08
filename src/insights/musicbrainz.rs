//! Credits from [MusicBrainz](https://musicbrainz.org), which needs no key.
//!
//! Each item takes two requests: a search that finds the best match for
//! what Spotify calls it, then a lookup of that match with its
//! relationships, which is where producers, engineers, players, studios and
//! writers are recorded. MusicBrainz asks for a descriptive User-Agent and at
//! most one request a second from each client; both are honoured here.
//!
//! The parsing below is separate from the requests so it can be tested
//! against small fixtures.

use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;

use super::{Kind, Subject};

const API: &str = "https://musicbrainz.org/ws/2";
const SITE: &str = "https://musicbrainz.org";
const USER_AGENT: &str = concat!(
    "Spotifast/",
    env!("CARGO_PKG_VERSION"),
    " ( https://github.com/alexing/spotifast )"
);
/// A little over MusicBrainz's one request a second.
const SPACING: Duration = Duration::from_millis(1100);
const TIMEOUT: Duration = Duration::from_secs(20);
/// Enough credits for a large record without flooding the prompt.
const MAX_CREDITS: usize = 300;
/// Albums listed for an artist.
const MAX_ALBUMS: usize = 30;

/// What MusicBrainz knows about the item.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Credits {
    /// The matched entity's page, which the prompt cites.
    pub url: String,
    /// Labelled facts: release date, labels, members' years.
    pub facts: Vec<(String, String)>,
    pub credits: Vec<Credit>,
}

/// One person or place in one role.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Credit {
    /// The relationship as MusicBrainz names it, with its attributes:
    /// "producer", "instrument (bass guitar)", "recorded at".
    pub role: String,
    pub name: String,
    /// The song the credit is on, when it is not the whole release.
    pub on: Option<String>,
}

/// Finds the item and reads its credits. `Ok(None)` when nothing matches
/// well enough to trust.
pub async fn lookup(http: &reqwest::Client, subject: &Subject) -> Result<Option<Credits>> {
    match subject.kind {
        Kind::Album => {
            let query = release_query(subject);
            let found = get(http, "release", &[("query", &query), ("limit", "25")]).await?;
            let Some(id) = best_release(&found, subject) else {
                return Ok(None);
            };
            let release = get(
                http,
                &format!("release/{id}"),
                &[(
                    "inc",
                    "artist-credits+labels+recordings+release-groups+artist-rels+place-rels+recording-level-rels+work-rels+work-level-rels",
                )],
            )
            .await?;
            Ok(Some(release_credits(&release)))
        }
        Kind::Track => {
            let query = recording_query(subject);
            let found = get(http, "recording", &[("query", &query), ("limit", "25")]).await?;
            let Some(id) = best_recording(&found, subject) else {
                return Ok(None);
            };
            let recording = get(
                http,
                &format!("recording/{id}"),
                &[(
                    "inc",
                    "artist-credits+releases+artist-rels+place-rels+work-rels+work-level-rels",
                )],
            )
            .await?;
            Ok(Some(recording_credits(&recording)))
        }
        Kind::Artist => {
            let query = format!("artist:{}", phrase(&subject.name));
            let found = get(http, "artist", &[("query", &query), ("limit", "10")]).await?;
            let Some(id) = best_artist(&found, subject) else {
                return Ok(None);
            };
            let artist = get(
                http,
                &format!("artist/{id}"),
                &[("inc", "artist-rels+release-groups")],
            )
            .await?;
            Ok(Some(artist_credits(&artist)))
        }
    }
}

/// One request, spaced from the previous one by at least [`SPACING`].
async fn get(http: &reqwest::Client, path: &str, params: &[(&str, &str)]) -> Result<Value> {
    static LAST: tokio::sync::Mutex<Option<tokio::time::Instant>> =
        tokio::sync::Mutex::const_new(None);
    for attempt in 0..2 {
        {
            let mut last = LAST.lock().await;
            if let Some(previous) = *last {
                tokio::time::sleep_until(previous + SPACING).await;
            }
            *last = Some(tokio::time::Instant::now());
        }
        let response = http
            .get(format!("{API}/{path}"))
            .query(params)
            .query(&[("fmt", "json")])
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .header(reqwest::header::ACCEPT, "application/json")
            .timeout(TIMEOUT)
            .send()
            .await
            .map_err(|error| error.without_url())
            .context("cannot reach MusicBrainz")?;
        let status = response.status();
        // 503 is MusicBrainz saying "slower": wait one slot and try again.
        if status == reqwest::StatusCode::SERVICE_UNAVAILABLE && attempt == 0 {
            continue;
        }
        if !status.is_success() {
            anyhow::bail!("MusicBrainz answered {status}");
        }
        return response
            .json()
            .await
            .context("unexpected answer from MusicBrainz");
    }
    anyhow::bail!("MusicBrainz is busy")
}

// ---- queries ----------------------------------------------------------------

/// A quoted Lucene phrase.
fn phrase(text: &str) -> String {
    let escaped = text.trim().replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn release_query(subject: &Subject) -> String {
    let mut query = format!(
        "release:{}",
        phrase(&crate::lyrics::clean_title(&subject.name))
    );
    if let Some(artist) = subject.main_artist() {
        query.push_str(&format!(" AND artist:{}", phrase(artist)));
    }
    query
}

fn recording_query(subject: &Subject) -> String {
    if let Some(isrc) = subject
        .isrc
        .as_deref()
        .filter(|isrc| !isrc.is_empty() && isrc.chars().all(|c| c.is_ascii_alphanumeric()))
    {
        return format!("isrc:{isrc}");
    }
    let mut query = format!(
        "recording:{}",
        phrase(&crate::lyrics::clean_title(&subject.name))
    );
    if let Some(artist) = subject.main_artist() {
        query.push_str(&format!(" AND artist:{}", phrase(artist)));
    }
    query
}

// ---- matching ---------------------------------------------------------------

/// Lower case letters and digits, words separated by single spaces, and
/// "&" read as "and".
pub fn normalize(text: &str) -> String {
    let text = text.to_lowercase().replace('&', " and ");
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// 2 for the same title, 1 when one contains the other, 0 otherwise.
fn title_match(candidate: &str, wanted: &str) -> u8 {
    let candidate = normalize(candidate);
    let wanted = normalize(&crate::lyrics::clean_title(wanted));
    if candidate.is_empty() || wanted.is_empty() {
        return 0;
    }
    if candidate == wanted {
        2
    } else if candidate.contains(&wanted) || wanted.contains(&candidate) {
        1
    } else {
        0
    }
}

/// The names in an `artist-credit` array, joined as MusicBrainz shows them.
fn credit_names(entity: &Value) -> String {
    entity
        .get("artist-credit")
        .and_then(Value::as_array)
        .map(|credits| {
            credits
                .iter()
                .map(|credit| {
                    let name = text(credit, "name");
                    let join = text(credit, "joinphrase");
                    format!("{name}{join}")
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Whether the credited artists include the subject's main artist. A
/// subject without artists matches anything.
fn artist_match(entity: &Value, subject: &Subject) -> bool {
    let Some(wanted) = subject.main_artist() else {
        return true;
    };
    let wanted = normalize(wanted);
    let credited = normalize(&credit_names(entity));
    !wanted.is_empty() && (credited.contains(&wanted) || wanted.contains(&credited))
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// A date for sorting: missing dates last.
fn date_key(date: &str) -> String {
    if date.is_empty() {
        "9999".into()
    } else {
        date.to_string()
    }
}

/// The best release among search results: the same title by the same
/// artist, official over anything else, an album over a single, and then
/// the earliest.
pub fn best_release(results: &Value, subject: &Subject) -> Option<String> {
    let releases = results.get("releases")?.as_array()?;
    releases
        .iter()
        .filter(|release| artist_match(release, subject))
        .filter_map(|release| {
            let title = title_match(text(release, "title"), &subject.name);
            (title > 0).then(|| {
                let official = text(release, "status") == "Official";
                let album = release
                    .pointer("/release-group/primary-type")
                    .and_then(Value::as_str)
                    == Some("Album");
                let score = release.get("score").and_then(Value::as_u64).unwrap_or(0);
                (
                    (title, official, album),
                    std::cmp::Reverse(date_key(text(release, "date"))),
                    score,
                    text(release, "id"),
                )
            })
        })
        .max_by(|a, b| (&a.0, &a.1, a.2).cmp(&(&b.0, &b.1, b.2)))
        .map(|(_, _, _, id)| id.to_string())
        .filter(|id| !id.is_empty())
}

/// The best recording: the same title by the same artist, the studio
/// original over a version with a disambiguation such as "live", and then
/// the earliest released.
pub fn best_recording(results: &Value, subject: &Subject) -> Option<String> {
    let recordings = results.get("recordings")?.as_array()?;
    recordings
        .iter()
        .filter(|recording| artist_match(recording, subject))
        .filter_map(|recording| {
            let title = title_match(text(recording, "title"), &subject.name);
            (title > 0).then(|| {
                let plain = text(recording, "disambiguation").is_empty();
                let score = recording.get("score").and_then(Value::as_u64).unwrap_or(0);
                (
                    (title, plain),
                    std::cmp::Reverse(date_key(text(recording, "first-release-date"))),
                    score,
                    text(recording, "id"),
                )
            })
        })
        .max_by(|a, b| (&a.0, &a.1, a.2).cmp(&(&b.0, &b.1, b.2)))
        .map(|(_, _, _, id)| id.to_string())
        .filter(|id| !id.is_empty())
}

/// The best artist: the same name, then MusicBrainz's own score.
pub fn best_artist(results: &Value, subject: &Subject) -> Option<String> {
    let artists = results.get("artists")?.as_array()?;
    artists
        .iter()
        .filter_map(|artist| {
            let name = normalize(text(artist, "name"));
            let wanted = normalize(&subject.name);
            let level = if name == wanted && !name.is_empty() {
                2
            } else if text(artist, "sort-name")
                .split(',')
                .rev()
                .map(normalize)
                .collect::<Vec<_>>()
                .join(" ")
                == wanted
            {
                1
            } else {
                0
            };
            (level > 0).then(|| {
                let score = artist.get("score").and_then(Value::as_u64).unwrap_or(0);
                (level, score, text(artist, "id"))
            })
        })
        .max_by_key(|(level, score, _)| (*level, *score))
        .map(|(_, _, id)| id.to_string())
        .filter(|id| !id.is_empty())
}

// ---- credits ----------------------------------------------------------------

/// Relationship types that say nothing about how the music was made.
const SKIPPED: &[&str] = &[
    "amazon asin",
    "discogs",
    "free streaming",
    "download for free",
    "purchase for download",
    "streaming",
    "license",
    "misc",
    "phonographic copyright",
    "copyright",
    "publishing",
    "rights society",
    "marketed",
    "distributed",
    "manufactured",
    "printed",
    "glass mastered",
    "pressed",
];

/// The credit a relationship records, if it is one worth telling.
fn relation_credit(relation: &Value, on: Option<&str>) -> Option<Credit> {
    let kind = text(relation, "type");
    if kind.is_empty() || SKIPPED.contains(&kind) {
        return None;
    }
    let name = match text(relation, "target-type") {
        "artist" => {
            let credited = text(relation, "target-credit");
            if credited.is_empty() {
                relation.pointer("/artist/name")?.as_str()?
            } else {
                credited
            }
        }
        "place" => relation.pointer("/place/name")?.as_str()?,
        _ => return None,
    };
    if name.trim().is_empty() {
        return None;
    }
    let attributes: Vec<&str> = relation
        .get("attributes")
        .and_then(Value::as_array)
        .map(|attributes| attributes.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let role = if attributes.is_empty() {
        kind.to_string()
    } else {
        format!("{kind} ({})", attributes.join(", "))
    };
    Some(Credit {
        role,
        name: name.to_string(),
        on: on.map(str::to_string),
    })
}

/// Credits on an entity's own relationships, and writers on the works it
/// records.
fn relations_credits(entity: &Value, on: Option<&str>, credits: &mut Vec<Credit>) {
    let Some(relations) = entity.get("relations").and_then(Value::as_array) else {
        return;
    };
    for relation in relations {
        if text(relation, "target-type") == "work" {
            if let Some(work) = relation.get("work") {
                relations_credits(work, on, credits);
            }
            continue;
        }
        if let Some(credit) = relation_credit(relation, on)
            && !credits.contains(&credit)
        {
            credits.push(credit);
        }
    }
}

fn push_fact(facts: &mut Vec<(String, String)>, label: &str, value: String) {
    if !value.trim().is_empty() {
        facts.push((label.to_string(), value));
    }
}

/// A release's facts and credits, its songs' included.
pub fn release_credits(release: &Value) -> Credits {
    let mut facts = Vec::new();
    push_fact(
        &mut facts,
        "Release",
        format!("{} by {}", text(release, "title"), credit_names(release)),
    );
    if let Some(group) = release.get("release-group") {
        push_fact(
            &mut facts,
            "First released",
            text(group, "first-release-date").to_string(),
        );
        push_fact(&mut facts, "Type", text(group, "primary-type").to_string());
    }
    push_fact(
        &mut facts,
        "This edition",
        [text(release, "date"), text(release, "country")]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(", "),
    );
    let mut labels: Vec<&str> = Vec::new();
    for info in release
        .get("label-info")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(name) = info.pointer("/label/name").and_then(Value::as_str)
            && !labels.contains(&name)
        {
            labels.push(name);
        }
    }
    push_fact(&mut facts, "Labels", labels.join(", "));

    let mut credits = Vec::new();
    relations_credits(release, None, &mut credits);
    for medium in release
        .get("media")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for track in medium
            .get("tracks")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let title = text(track, "title");
            if let Some(recording) = track.get("recording") {
                let title = if title.is_empty() {
                    text(recording, "title")
                } else {
                    title
                };
                relations_credits(recording, Some(title), &mut credits);
            }
        }
    }
    credits.truncate(MAX_CREDITS);
    Credits {
        url: format!("{SITE}/release/{}", text(release, "id")),
        facts,
        credits,
    }
}

/// A recording's facts and credits.
pub fn recording_credits(recording: &Value) -> Credits {
    let mut facts = Vec::new();
    push_fact(
        &mut facts,
        "Recording",
        format!(
            "{} by {}",
            text(recording, "title"),
            credit_names(recording)
        ),
    );
    push_fact(
        &mut facts,
        "First released",
        text(recording, "first-release-date").to_string(),
    );
    push_fact(
        &mut facts,
        "Version",
        text(recording, "disambiguation").to_string(),
    );
    let mut releases: Vec<&str> = Vec::new();
    for release in recording
        .get("releases")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let title = text(release, "title");
        if !title.is_empty() && !releases.contains(&title) && releases.len() < 8 {
            releases.push(title);
        }
    }
    push_fact(&mut facts, "Appears on", releases.join("; "));
    let mut credits = Vec::new();
    relations_credits(recording, None, &mut credits);
    credits.truncate(MAX_CREDITS);
    Credits {
        url: format!("{SITE}/recording/{}", text(recording, "id")),
        facts,
        credits,
    }
}

/// Relationships between artists that say something about the music.
const ARTIST_LINKS: &[&str] = &[
    "member of band",
    "collaboration",
    "subgroup",
    "supporting musician",
    "vocal supporting musician",
    "instrumental supporting musician",
    "conductor position",
    "founder",
];

/// An artist's facts, members and studio albums.
pub fn artist_credits(artist: &Value) -> Credits {
    let mut facts = Vec::new();
    push_fact(&mut facts, "Artist", text(artist, "name").to_string());
    push_fact(&mut facts, "Kind", text(artist, "type").to_string());
    push_fact(
        &mut facts,
        "Note",
        text(artist, "disambiguation").to_string(),
    );
    let area = artist
        .pointer("/area/name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let from = artist
        .pointer("/begin-area/name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    push_fact(&mut facts, "Area", area.to_string());
    if from != area {
        push_fact(&mut facts, "From", from.to_string());
    }
    if let Some(span) = artist.get("life-span") {
        push_fact(&mut facts, "Active", years(span));
    }
    let mut albums: Vec<(String, String)> = artist
        .get("release-groups")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|group| {
            text(group, "primary-type") == "Album"
                && group
                    .get("secondary-types")
                    .and_then(Value::as_array)
                    .is_none_or(|types| types.is_empty())
        })
        .map(|group| {
            (
                text(group, "first-release-date").to_string(),
                text(group, "title").to_string(),
            )
        })
        .collect();
    albums.sort();
    albums.dedup_by(|a, b| a.1 == b.1);
    push_fact(
        &mut facts,
        "Studio albums",
        albums
            .iter()
            .take(MAX_ALBUMS)
            .map(|(date, title)| match date.get(..4) {
                Some(year) => format!("{title} ({year})"),
                None => title.clone(),
            })
            .collect::<Vec<_>>()
            .join("; "),
    );

    let mut credits = Vec::new();
    for relation in artist
        .get("relations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let kind = text(relation, "type");
        if text(relation, "target-type") != "artist" || !ARTIST_LINKS.contains(&kind) {
            continue;
        }
        let Some(name) = relation.pointer("/artist/name").and_then(Value::as_str) else {
            continue;
        };
        // "backward" points from the other artist to this one: a member of
        // this band, rather than a band this artist is in.
        let role = match (kind, text(relation, "direction")) {
            ("member of band", "backward") => "member".to_string(),
            ("member of band", _) => "member of".to_string(),
            (kind, "backward") => format!("{kind} (of this artist)"),
            (kind, _) => kind.to_string(),
        };
        let attributes: Vec<&str> = relation
            .get("attributes")
            .and_then(Value::as_array)
            .map(|attributes| attributes.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let role = if attributes.is_empty() {
            role
        } else {
            format!("{role} ({})", attributes.join(", "))
        };
        let span = years(relation);
        let name = if span.is_empty() {
            name.to_string()
        } else {
            format!("{name} ({span})")
        };
        credits.push(Credit {
            role,
            name,
            on: None,
        });
    }
    credits.truncate(MAX_CREDITS);
    Credits {
        url: format!("{SITE}/artist/{}", text(artist, "id")),
        facts,
        credits,
    }
}

/// "1978-2016" from a `begin` and an `end`, either of which may be absent.
fn years(span: &Value) -> String {
    let year = |key: &str| text(span, key).get(..4).unwrap_or_default().to_string();
    match (year("begin"), year("end")) {
        (begin, end) if begin.is_empty() && end.is_empty() => String::new(),
        (begin, end) if end.is_empty() => format!("{begin}-"),
        (begin, end) => format!("{begin}-{end}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn subject(kind: Kind, name: &str, artist: Option<&str>) -> Subject {
        let mut subject = Subject::from_uri(&format!("spotify:{}:x", kind.as_str()), name).unwrap();
        subject.artists = artist.map(|a| vec![a.to_string()]).unwrap_or_default();
        subject
    }

    fn credit(name: &str) -> Value {
        json!([{ "name": name, "joinphrase": "", "artist": { "name": name } }])
    }

    #[test]
    fn names_normalize_for_comparison() {
        assert_eq!(
            normalize("Sign \u{2019}O\u{2019} the Times"),
            "sign o the times"
        );
        assert_eq!(normalize("Simon & Garfunkel"), "simon and garfunkel");
        assert_eq!(normalize("  AC/DC  "), "ac dc");
    }

    #[test]
    fn queries_quote_and_escape_names() {
        let album = subject(
            Kind::Album,
            "Say \"Hi\" (Remastered 2011)",
            Some("Back\\slash"),
        );
        assert_eq!(
            release_query(&album),
            "release:\"Say \\\"Hi\\\"\" AND artist:\"Back\\\\slash\""
        );
        let mut song = subject(Kind::Track, "It", Some("Prince"));
        assert_eq!(
            recording_query(&song),
            "recording:\"It\" AND artist:\"Prince\""
        );
        song.isrc = Some("USWB18700123".into());
        assert_eq!(recording_query(&song), "isrc:USWB18700123");
        song.isrc = Some("bad isrc\"".into());
        assert!(recording_query(&song).starts_with("recording:"));
    }

    #[test]
    fn the_earliest_official_exact_release_wins() {
        let results = json!({ "releases": [
            { "id": "bootleg", "score": 100, "title": "Sign o' the Times", "status": "Bootleg",
              "date": "1986-01-01", "artist-credit": credit("Prince"),
              "release-group": { "primary-type": "Album" } },
            { "id": "deluxe", "score": 100, "title": "Sign o' the Times (Super Deluxe Edition)",
              "status": "Official", "date": "2020-09-25", "artist-credit": credit("Prince"),
              "release-group": { "primary-type": "Album" } },
            { "id": "reissue", "score": 100, "title": "Sign \u{2019}O\u{2019} the Times", "status": "Official",
              "date": "1990", "artist-credit": credit("Prince"),
              "release-group": { "primary-type": "Album" } },
            { "id": "original", "score": 98, "title": "Sign \u{2019}O\u{2019} the Times", "status": "Official",
              "date": "1987-03-30", "artist-credit": credit("Prince"),
              "release-group": { "primary-type": "Album" } },
            { "id": "single", "score": 100, "title": "Sign o' the Times", "status": "Official",
              "date": "1987-02-18", "artist-credit": credit("Prince"),
              "release-group": { "primary-type": "Single" } },
            { "id": "tribute", "score": 100, "title": "Sign o' the Times", "status": "Official",
              "date": "1980", "artist-credit": credit("Somebody Else"),
              "release-group": { "primary-type": "Album" } }
        ]});
        let wanted = subject(
            Kind::Album,
            "Sign o' the Times (Remastered)",
            Some("Prince"),
        );
        assert_eq!(best_release(&results, &wanted).as_deref(), Some("original"));
    }

    #[test]
    fn nothing_close_enough_is_no_match() {
        let results = json!({ "releases": [
            { "id": "x", "title": "Purple Rain", "status": "Official", "artist-credit": credit("Prince") }
        ]});
        let wanted = subject(Kind::Album, "Lovesexy", Some("Prince"));
        assert_eq!(best_release(&results, &wanted), None);
        assert_eq!(best_release(&json!({}), &wanted), None);
    }

    #[test]
    fn the_studio_original_recording_beats_live_versions() {
        let results = json!({ "recordings": [
            { "id": "live", "score": 100, "title": "U Got the Look", "disambiguation": "live, 1988",
              "first-release-date": "1988", "artist-credit": credit("Prince") },
            { "id": "remix", "score": 100, "title": "U Got the Look (Long Look)",
              "first-release-date": "1987-07", "artist-credit": credit("Prince") },
            { "id": "studio", "score": 95, "title": "U Got the Look",
              "first-release-date": "1987-03-30", "artist-credit": json!([
                  { "name": "Prince", "joinphrase": " & " }, { "name": "Sheena Easton", "joinphrase": "" }
              ]) }
        ]});
        let wanted = subject(Kind::Track, "U Got the Look", Some("Prince"));
        assert_eq!(best_recording(&results, &wanted).as_deref(), Some("studio"));
    }

    #[test]
    fn artists_match_by_name_or_sort_name() {
        let results = json!({ "artists": [
            { "id": "tribute", "score": 100, "name": "Prince Tribute Band", "sort-name": "Prince Tribute Band" },
            { "id": "prince", "score": 99, "name": "Prince", "sort-name": "Prince" },
            { "id": "other", "score": 60, "name": "Prince", "sort-name": "Prince", "disambiguation": "rapper" }
        ]});
        assert_eq!(
            best_artist(&results, &subject(Kind::Artist, "Prince", None)).as_deref(),
            Some("prince")
        );
        let beatles = json!({ "artists": [
            { "id": "b", "score": 100, "name": "Beatles, The", "sort-name": "Beatles, The" }
        ]});
        assert_eq!(
            best_artist(&beatles, &subject(Kind::Artist, "The Beatles", None)).as_deref(),
            Some("b")
        );
    }

    fn release_fixture() -> Value {
        json!({
            "id": "rel1",
            "title": "Sign \u{2019}O\u{2019} the Times",
            "date": "1987-03-30",
            "country": "US",
            "artist-credit": credit("Prince"),
            "release-group": { "first-release-date": "1987-03-30", "primary-type": "Album" },
            "label-info": [
                { "label": { "name": "Paisley Park" } },
                { "label": { "name": "Warner Bros. Records" } },
                { "label": { "name": "Paisley Park" } }
            ],
            "relations": [
                { "type": "producer", "target-type": "artist", "attributes": [], "artist": { "name": "Prince" } },
                { "type": "recorded at", "target-type": "place", "place": { "name": "Sunset Sound" } },
                { "type": "amazon asin", "target-type": "url", "url": { "resource": "https://amazon" } },
                { "type": "phonographic copyright", "target-type": "label", "label": { "name": "Warner" } }
            ],
            "media": [{ "tracks": [
                { "title": "U Got the Look", "recording": {
                    "title": "U Got the Look",
                    "relations": [
                        { "type": "instrument", "target-type": "artist", "attributes": ["drums"],
                          "artist": { "name": "Sheila Escovedo" }, "target-credit": "Sheila E." },
                        { "type": "vocal", "target-type": "artist", "attributes": ["guest"],
                          "artist": { "name": "Sheena Easton" } },
                        { "type": "performance", "target-type": "work", "work": {
                            "title": "U Got the Look",
                            "relations": [
                                { "type": "writer", "target-type": "artist", "artist": { "name": "Prince" } }
                            ]
                        } }
                    ]
                } },
                { "title": "Housequake", "recording": { "relations": [
                    { "type": "instrument", "target-type": "artist", "attributes": ["saxophone"],
                      "artist": { "name": "Eric Leeds" } }
                ] } }
            ] }]
        })
    }

    #[test]
    fn release_credits_cover_the_record_and_its_songs() {
        let credits = release_credits(&release_fixture());
        assert_eq!(credits.url, "https://musicbrainz.org/release/rel1");
        assert!(credits.facts.contains(&(
            "Labels".to_string(),
            "Paisley Park, Warner Bros. Records".to_string()
        )));
        assert!(credits.facts.contains(&(
            "Release".to_string(),
            "Sign \u{2019}O\u{2019} the Times by Prince".to_string()
        )));
        assert!(
            credits
                .facts
                .contains(&("This edition".to_string(), "1987-03-30, US".to_string()))
        );
        let shown: Vec<(String, String, Option<String>)> = credits
            .credits
            .into_iter()
            .map(|credit| (credit.role, credit.name, credit.on))
            .collect();
        let on = |song: &str| Some(song.to_string());
        assert_eq!(
            shown,
            [
                ("producer".into(), "Prince".into(), None),
                ("recorded at".into(), "Sunset Sound".into(), None),
                (
                    "instrument (drums)".into(),
                    "Sheila E.".into(),
                    on("U Got the Look")
                ),
                (
                    "vocal (guest)".into(),
                    "Sheena Easton".into(),
                    on("U Got the Look")
                ),
                ("writer".into(), "Prince".into(), on("U Got the Look")),
                (
                    "instrument (saxophone)".into(),
                    "Eric Leeds".into(),
                    on("Housequake")
                ),
            ]
        );
    }

    #[test]
    fn recording_credits_include_writers_and_releases() {
        let recording = json!({
            "id": "rec1",
            "title": "Sign o' the Times",
            "first-release-date": "1987-02-18",
            "artist-credit": credit("Prince"),
            "releases": [ { "title": "Sign o' the Times" }, { "title": "Sign o' the Times" }, { "title": "The Hits 1" } ],
            "relations": [
                { "type": "producer", "target-type": "artist", "artist": { "name": "Prince" } },
                { "type": "performance", "target-type": "work", "work": { "relations": [
                    { "type": "composer", "target-type": "artist", "artist": { "name": "Prince" } }
                ] } }
            ]
        });
        let credits = recording_credits(&recording);
        assert_eq!(credits.url, "https://musicbrainz.org/recording/rec1");
        assert!(credits.facts.contains(&(
            "Appears on".to_string(),
            "Sign o' the Times; The Hits 1".to_string()
        )));
        assert_eq!(credits.credits.len(), 2);
        assert_eq!(credits.credits[1].role, "composer");
    }

    #[test]
    fn artist_credits_list_members_and_studio_albums_in_order() {
        let artist = json!({
            "id": "art1",
            "name": "The Revolution",
            "type": "Group",
            "area": { "name": "United States" },
            "begin-area": { "name": "Minneapolis" },
            "life-span": { "begin": "1979", "end": "1986-10" },
            "relations": [
                { "type": "member of band", "direction": "backward", "target-type": "artist",
                  "attributes": ["guitar"], "begin": "1983", "end": "1986",
                  "artist": { "name": "Wendy Melvoin" } },
                { "type": "married", "direction": "forward", "target-type": "artist",
                  "artist": { "name": "Someone" } }
            ],
            "release-groups": [
                { "title": "Purple Rain", "primary-type": "Album", "secondary-types": ["Soundtrack"], "first-release-date": "1984-06-25" },
                { "title": "Parade", "primary-type": "Album", "secondary-types": [], "first-release-date": "1986-03-31" },
                { "title": "Around the World in a Day", "primary-type": "Album", "first-release-date": "1985-04-22" },
                { "title": "Kiss", "primary-type": "Single", "first-release-date": "1986-02-05" }
            ]
        });
        let credits = artist_credits(&artist);
        assert!(
            credits
                .facts
                .contains(&("Active".to_string(), "1979-1986".to_string()))
        );
        assert!(
            credits
                .facts
                .contains(&("From".to_string(), "Minneapolis".to_string()))
        );
        assert!(credits.facts.contains(&(
            "Studio albums".to_string(),
            "Around the World in a Day (1985); Parade (1986)".to_string()
        )));
        assert_eq!(
            credits.credits,
            [Credit {
                role: "member (guitar)".into(),
                name: "Wendy Melvoin (1983-1986)".into(),
                on: None
            }]
        );
    }

    #[test]
    fn spans_read_as_years() {
        assert_eq!(
            years(&json!({ "begin": "1978-06-07", "end": "2016-04-21" })),
            "1978-2016"
        );
        assert_eq!(years(&json!({ "begin": "1990" })), "1990-");
        assert_eq!(years(&json!({})), "");
    }

    #[test]
    fn the_user_agent_identifies_the_app() {
        assert!(USER_AGENT.starts_with("Spotifast/"));
        assert!(USER_AGENT.ends_with("( https://github.com/alexing/spotifast )"));
    }
}
