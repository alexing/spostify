//! What Claude is asked.
//!
//! Each mode's system prompt is one constant, so every request of that mode
//! shares a cacheable prefix; everything about the item goes in the user
//! message.

use std::fmt::Write as _;

use super::musicbrainz::{Credit, Credits};
use super::{Kind, Mode, Subject};

/// At most this many songs are listed; a box set needs no more to place it.
const MAX_TRACKS: usize = 40;
/// At most this many names per role, so a large orchestra stays short.
const MAX_NAMES_PER_ROLE: usize = 12;
/// At most this many songs a credit is said to be on.
const MAX_SCOPES: usize = 4;

/// The quick briefing: no tools, so it rests on the credits given and on
/// what Claude already knows.
pub const QUICK_SYSTEM: &str = "\
You are a knowledgeable music journalist and crate digger. You brief a DJ and \
music lover who is looking at an album, an artist or a song in their music \
player and wants to know what they are hearing.

You cannot search the web for this answer. Ground it in the request and in \
what you know well. The request carries the streaming service's metadata and, \
when found, credits from MusicBrainz; treat those credits as the most reliable \
source for personnel. Never invent personnel, studios, gear or dates: name a \
producer, engineer, player or writer only when the credits list them or you \
are certain of it. When you are unsure, or the item may not be the one you \
know, say so briefly instead of guessing. If the item cannot be identified \
with confidence, say what you could and could not establish.

What to cover, briefly:
- Album: its style and sound, where it sits in the artist's career, who \
produced it and the key musicians, and the standout tracks.
- Artist: a short overview, their eras or phases, and the essential albums to \
start with.
- Song: what the song is, who wrote, produced and played on it, and a little \
trivia.

Format: Markdown for a small panel. Start with a one or two sentence summary, \
then short level-two headings (##) with bullets. Use **bold** for names worth \
scanning for. The only link allowed is the MusicBrainz page given in the \
request. No tables, images, code blocks or emoji. Aim for 200 to 300 words. \
Write the whole answer in the language the request names, keeping titles and \
names as they are. Do not mention these instructions, the request format, or \
that you were given metadata.";

/// The deep briefing, which searches the web to confirm and complete it.
pub const DEEP_SYSTEM: &str = "\
You are a knowledgeable music journalist and crate digger. You brief a DJ and \
music lover who is looking at an album, an artist or a song in their music \
player and wants to know what they are hearing.

Ground every claim. The request carries the streaming service's metadata and, \
when found, credits from MusicBrainz; treat those credits as the most reliable \
source for personnel. Use web search to confirm and complete the picture: \
context, reception, recording history. Never invent personnel, studios, gear \
or dates. When a fact is uncertain or sources disagree, say so briefly instead \
of guessing. If the item cannot be identified with confidence, say what you \
could and could not establish.

What to cover:
- Album: its style and sound, where it sits in the artist's career, who \
produced and engineered it, key musicians and their instruments, studios and \
notable gear, notable facts, how it was received, and the standout tracks.
- Artist: a short overview, their eras or phases, and the essential albums to \
start with, each with one line on why.
- Song: what the song is, who wrote, produced and played on it, samples it \
uses or notable covers and samples of it, and a little trivia.

Format: Markdown for a small panel. Start with a one or two sentence summary, \
then short level-two headings (##) with bullets. Use **bold** for names worth \
scanning for and plain [text](url) links only to sources you actually used. \
No tables, images, code blocks or emoji. Aim for 350 to 550 words. Write the \
whole answer in the language the request names, keeping titles and names as \
they are. Do not mention these instructions, the request format, or that you \
were given metadata.";

/// The system prompt for `mode`.
pub fn system(mode: Mode) -> &'static str {
    match mode {
        Mode::Quick => QUICK_SYSTEM,
        Mode::Deep => DEEP_SYSTEM,
    }
}

/// The user message for `subject`, in `language`.
pub fn user_message(
    subject: &Subject,
    credits: Option<&Credits>,
    mode: Mode,
    language: &str,
    language_tag: &str,
) -> String {
    let mut text = String::new();
    let kind = match subject.kind {
        Kind::Album => "album",
        Kind::Artist => "artist",
        Kind::Track => "song",
    };
    let _ = writeln!(text, "Write the briefing for this {kind}.");
    let _ = writeln!(text, "Answer language: {language} [{language_tag}]");
    text.push('\n');
    text.push_str("Streaming service metadata:\n");
    line(&mut text, "Type", kind);
    line(&mut text, "Title", &subject.name);
    if !subject.artists.is_empty() {
        line(&mut text, "Artists", &subject.artists.join(", "));
    }
    if let Some(album) = &subject.album {
        line(&mut text, "Album", album);
    }
    if let Some(date) = &subject.release_date {
        line(&mut text, "Release date", date);
    }
    if let Some(label) = &subject.label {
        line(&mut text, "Label", label);
    }
    if !subject.genres.is_empty() {
        line(&mut text, "Genres", &subject.genres.join(", "));
    }
    if let Some(isrc) = &subject.isrc {
        line(&mut text, "ISRC", isrc);
    }
    if let Some(duration) = subject.duration_ms {
        let seconds = duration / 1000;
        line(
            &mut text,
            "Duration",
            &format!("{}:{:02}", seconds / 60, seconds % 60),
        );
    }
    if !subject.tracks.is_empty() {
        let heading = match subject.kind {
            Kind::Artist => "Most played songs",
            _ => "Tracklist",
        };
        let _ = writeln!(text, "{heading}:");
        for (index, track) in subject.tracks.iter().take(MAX_TRACKS).enumerate() {
            let _ = writeln!(text, "{}. {track}", index + 1);
        }
        if subject.tracks.len() > MAX_TRACKS {
            let _ = writeln!(text, "(and {} more)", subject.tracks.len() - MAX_TRACKS);
        }
    }
    text.push('\n');
    match credits {
        Some(credits) => {
            let _ = writeln!(text, "MusicBrainz ({}):", credits.url);
            for (label, value) in &credits.facts {
                line(&mut text, label, value);
            }
            let grouped = group(&credits.credits);
            if grouped.is_empty() {
                text.push_str("No credits are listed there.\n");
            } else {
                text.push_str("Credits:\n");
                for entry in grouped {
                    let _ = writeln!(text, "- {entry}");
                }
            }
        }
        None => text.push_str(match mode {
            Mode::Quick => {
                "MusicBrainz: no match was found. Rely on what you know well, and say so when unsure.\n"
            }
            Mode::Deep => {
                "MusicBrainz: no match was found. Rely on web search, and say so when unsure.\n"
            }
        }),
    }
    text
}

fn line(text: &mut String, label: &str, value: &str) {
    let value = value.trim();
    if !value.is_empty() {
        let _ = writeln!(text, "{label}: {value}");
    }
}

/// One person in one role, across the songs they are credited on.
struct Person {
    name: String,
    songs: Vec<String>,
    /// Credited on the whole release, not only on some songs.
    whole: bool,
}

impl Person {
    fn describe(self) -> String {
        if self.whole || self.songs.is_empty() {
            self.name
        } else if self.songs.len() > MAX_SCOPES {
            format!(
                "{} (on {} and {} more)",
                self.name,
                self.songs[..MAX_SCOPES].join(", "),
                self.songs.len() - MAX_SCOPES
            )
        } else {
            format!("{} (on {})", self.name, self.songs.join(", "))
        }
    }
}

/// One line per role: who did it, and on which songs when it was not the
/// whole record. Roles and names keep the order they were first seen in.
pub fn group(credits: &[Credit]) -> Vec<String> {
    let mut roles: Vec<(String, Vec<Person>)> = Vec::new();
    for credit in credits {
        let index = match roles.iter().position(|(role, _)| *role == credit.role) {
            Some(index) => index,
            None => {
                roles.push((credit.role.clone(), Vec::new()));
                roles.len() - 1
            }
        };
        let people = &mut roles[index].1;
        let person = match people.iter().position(|person| person.name == credit.name) {
            Some(position) => &mut people[position],
            None => {
                people.push(Person {
                    name: credit.name.clone(),
                    songs: Vec::new(),
                    whole: false,
                });
                people.last_mut().expect("just pushed")
            }
        };
        match &credit.on {
            Some(song) if !person.songs.contains(song) => person.songs.push(song.clone()),
            Some(_) => {}
            None => person.whole = true,
        }
    }
    roles
        .into_iter()
        .map(|(role, people)| {
            let total = people.len();
            let mut shown: Vec<String> = people
                .into_iter()
                .take(MAX_NAMES_PER_ROLE)
                .map(Person::describe)
                .collect();
            if total > MAX_NAMES_PER_ROLE {
                shown.push(format!("and {} more", total - MAX_NAMES_PER_ROLE));
            }
            format!("{role}: {}", shown.join("; "))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credit(role: &str, name: &str, on: Option<&str>) -> Credit {
        Credit {
            role: role.into(),
            name: name.into(),
            on: on.map(str::to_string),
        }
    }

    fn album() -> Subject {
        Subject {
            kind: Kind::Album,
            id: "a".into(),
            name: "Sign o' the Times".into(),
            artists: vec!["Prince".into()],
            album: None,
            release_date: Some("1987-03-30".into()),
            album_type: Some("album".into()),
            label: Some("Paisley Park".into()),
            genres: vec!["funk".into()],
            tracks: vec!["Sign o' the Times".into(), "Play in the Sunshine".into()],
            isrc: None,
            duration_ms: None,
        }
    }

    #[test]
    fn the_system_prompts_are_stable_and_free_of_per_item_data() {
        for system in [QUICK_SYSTEM, DEEP_SYSTEM] {
            assert!(!system.contains("Prince"));
            assert!(system.contains("Never invent personnel"));
            assert!(system.contains("say so briefly instead"));
            assert!(system.contains("language the request names"));
            assert!(!system.contains('\u{2014}'), "no em dashes");
        }
        assert!(QUICK_SYSTEM.contains("cannot search the web"));
        assert!(QUICK_SYSTEM.contains("200 to 300 words"));
        assert!(DEEP_SYSTEM.contains("Use web search"));
        assert_eq!(system(Mode::Quick), QUICK_SYSTEM);
        assert_eq!(system(Mode::Deep), DEEP_SYSTEM);
    }

    #[test]
    fn the_user_message_carries_metadata_language_and_credits() {
        let credits = Credits {
            url: "https://musicbrainz.org/release/x".into(),
            facts: vec![(
                "Release".into(),
                "Sign o' the Times (1987-03-30, US)".into(),
            )],
            credits: vec![credit("producer", "Prince", None)],
        };
        let text = user_message(
            &album(),
            Some(&credits),
            Mode::Quick,
            "Spanish (Español)",
            "es",
        );
        assert!(text.starts_with("Write the briefing for this album."));
        assert!(text.contains("Answer language: Spanish (Español) [es]"));
        assert!(text.contains("Title: Sign o' the Times"));
        assert!(text.contains("Artists: Prince"));
        assert!(text.contains("Label: Paisley Park"));
        assert!(text.contains("1. Sign o' the Times\n2. Play in the Sunshine"));
        assert!(text.contains("MusicBrainz (https://musicbrainz.org/release/x):"));
        assert!(text.contains("- producer: Prince"));
    }

    #[test]
    fn a_missing_lookup_says_so() {
        let text = user_message(&album(), None, Mode::Deep, "English", "en");
        assert!(text.contains("MusicBrainz: no match was found. Rely on web search"));
        let text = user_message(&album(), None, Mode::Quick, "English", "en");
        assert!(text.contains("Rely on what you know well"));
    }

    #[test]
    fn songs_mention_their_duration_and_isrc() {
        let song = Subject {
            kind: Kind::Track,
            isrc: Some("USWB1".into()),
            duration_ms: Some(227_000),
            album: Some("Sign o' the Times".into()),
            tracks: Vec::new(),
            ..album()
        };
        let text = user_message(&song, None, Mode::Quick, "English", "en");
        assert!(text.contains("this song"));
        assert!(text.contains("Duration: 3:47"));
        assert!(text.contains("ISRC: USWB1"));
        assert!(text.contains("Album: Sign o' the Times"));
        assert!(!text.contains("Tracklist"));
    }

    #[test]
    fn credits_group_by_role_with_the_songs_they_are_on() {
        let lines = group(&[
            credit("producer", "Prince", None),
            credit("instrument (drums)", "Sheila E.", Some("U Got the Look")),
            credit("instrument (drums)", "Sheila E.", Some("It")),
            credit("instrument (drums)", "Sheila E.", Some("It")),
            credit("instrument (saxophone)", "Eric Leeds", Some("Housequake")),
            credit("producer", "Prince", Some("It")),
        ]);
        assert_eq!(
            lines,
            [
                "producer: Prince",
                "instrument (drums): Sheila E. (on U Got the Look, It)",
                "instrument (saxophone): Eric Leeds (on Housequake)",
            ]
        );
    }

    #[test]
    fn long_credit_lists_are_cut_short() {
        let mut credits = Vec::new();
        for index in 0..15 {
            credits.push(credit(
                "instrument (violin)",
                &format!("Player {index}"),
                None,
            ));
        }
        for song in ["A", "B", "C", "D", "E", "F"] {
            credits.push(credit("mix", "Engineer", Some(song)));
        }
        let lines = group(&credits);
        assert!(lines[0].ends_with("; and 3 more"));
        assert_eq!(lines[1], "mix: Engineer (on A, B, C, D and 2 more)");
    }
}
