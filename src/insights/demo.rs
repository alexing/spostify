//! A canned briefing for `--demo` and tests, so the panel can be tried
//! without a network or an API key. It is played into the panel a few words
//! at a time, the way a real one streams in.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::{Insight, Kind, Mode, Phase, Progress, Subject};

/// The pause between pieces of the sample.
const PIECE: Duration = Duration::from_millis(35);
/// Words per piece.
const WORDS: usize = 3;

/// A plausible briefing for `subject`, plainly marked as a sample.
pub fn insight(subject: &Subject, mode: Mode) -> Insight {
    let mut markdown = quick(subject);
    if mode == Mode::Deep {
        markdown.push_str(
            "\n\n## Recording history\n\
             - Sessions ran over two winters, with overdubs added at home\n\
             - The engineer has said the drums were miked from the balcony\n\n\
             ## Sources\n\
             - [MusicBrainz](https://musicbrainz.org/)\n\
             - [Wikipedia](https://en.wikipedia.org/)",
        );
    }
    Insight {
        markdown,
        created_at: jiff::Timestamp::now().as_second(),
        truncated: false,
        mode,
        cached: false,
    }
}

fn quick(subject: &Subject) -> String {
    let artist = subject.main_artist().unwrap_or("the artist");
    let name = &subject.name;
    match subject.kind {
        Kind::Album => format!(
            "*Sample insight: demo mode does not contact Claude.*\n\n\
             **{name}** is the record where {artist} traded club polish for \
             something warmer and more patient, built from live takes and \
             tape loops.\n\n\
             ## Sound and context\n\
             - Downtempo grooves with live bass and brushed drums\n\
             - Written on the road between two long tours\n\n\
             ## Who made it\n\
             - **Produced by** {artist}, mixed by a long-time collaborator\n\
             - Strings arranged for a small chamber ensemble\n\
             - Recorded in a converted church studio on a vintage console\n\n\
             ## Reception\n\
             - Critics praised its restraint; fans call it a grower\n\n\
             ## Start with\n\
             1. The opening track, for the mood\n\
             2. The closing track, for the strings\n\n\
             Source: [MusicBrainz](https://musicbrainz.org/)"
        ),
        Kind::Artist => format!(
            "*Sample insight: demo mode does not contact Claude.*\n\n\
             **{name}** moves between electronic music and live \
             instrumentation, and rewards listening in order.\n\n\
             ## Eras\n\
             - **Early records**: bedroom-produced beats and samples\n\
             - **Middle period**: a touring band and bigger arrangements\n\
             - **Recent work**: quieter, more personal songs\n\n\
             ## Where to start\n\
             1. The breakthrough album, for the hooks\n\
             2. The live record, for the band\n\n\
             Source: [MusicBrainz](https://musicbrainz.org/)"
        ),
        Kind::Track => format!(
            "*Sample insight: demo mode does not contact Claude.*\n\n\
             **{name}** by {artist} is built around a looped bass figure and \
             a vocal recorded in one take.\n\n\
             ## Credits\n\
             - **Written by** {artist}\n\
             - Drums played live, then resampled\n\n\
             ## Trivia\n\
             - The title came from a note on the studio whiteboard\n\n\
             Source: [MusicBrainz](https://musicbrainz.org/)"
        ),
    }
}

/// A sample being played into the panel.
#[derive(Debug)]
pub struct Stream {
    id: u64,
    insight: Insight,
    /// What is still to come, and when.
    queue: VecDeque<(Instant, Progress)>,
}

impl Stream {
    pub fn new(id: u64, insight: Insight, now: Instant) -> Self {
        let mut queue = VecDeque::new();
        let mut at = now;
        let mut push = |delay: Duration, progress: Progress| {
            at += delay;
            queue.push_back((at, progress));
        };
        push(Duration::ZERO, Progress::Phase(Phase::Credits));
        push(Duration::from_millis(400), Progress::Phase(Phase::Writing));
        if insight.mode == Mode::Deep {
            push(
                Duration::from_millis(300),
                Progress::Phase(Phase::Searching),
            );
            push(Duration::from_millis(900), Progress::Phase(Phase::Writing));
        }
        let mut words = 0;
        let mut piece = String::new();
        for word in insight.markdown.split_inclusive(char::is_whitespace) {
            piece.push_str(word);
            words += 1;
            if words == WORDS {
                push(PIECE, Progress::Text(std::mem::take(&mut piece)));
                words = 0;
            }
        }
        if !piece.is_empty() {
            push(PIECE, Progress::Text(piece));
        }
        Self { id, insight, queue }
    }

    /// The request it answers, and the news due by `now`.
    pub fn due(&mut self, now: Instant) -> (u64, Vec<Progress>) {
        let mut due = Vec::new();
        while self.queue.front().is_some_and(|(at, _)| *at <= now) {
            due.extend(self.queue.pop_front().map(|(_, progress)| progress));
        }
        (self.id, due)
    }

    pub fn is_done(&self) -> bool {
        self.queue.is_empty()
    }

    pub fn into_result(self) -> (u64, Insight) {
        (self.id, self.insight)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_has_a_sample_that_parses() {
        for uri in ["spotify:album:a", "spotify:artist:a", "spotify:track:a"] {
            let subject = Subject::from_uri(uri, "Fragments").unwrap();
            let insight = insight(&subject, Mode::Quick);
            assert!(insight.markdown.contains("Fragments"));
            let deep = super::insight(&subject, Mode::Deep);
            assert!(deep.markdown.starts_with(&insight.markdown));
            assert!(deep.markdown.contains("## Sources"));
            let blocks = super::super::markdown::parse(&deep.markdown);
            assert!(blocks.iter().any(|block| matches!(
                block,
                super::super::markdown::Block::Heading { level: 2, .. }
            )));
        }
    }

    #[test]
    fn the_stream_hands_out_the_whole_sample_in_order() {
        let subject = Subject::from_uri("spotify:track:a", "Fragments").unwrap();
        let sample = insight(&subject, Mode::Deep);
        let start = Instant::now();
        let mut stream = Stream::new(7, sample.clone(), start);
        let (id, first) = stream.due(start);
        assert_eq!(id, 7);
        assert_eq!(first, [Progress::Phase(Phase::Credits)]);
        let (_, rest) = stream.due(start + Duration::from_secs(3600));
        assert!(stream.is_done());
        assert!(rest.contains(&Progress::Phase(Phase::Searching)));
        let text: String = rest
            .into_iter()
            .filter_map(|progress| match progress {
                Progress::Text(text) => Some(text),
                Progress::Phase(_) => None,
            })
            .collect();
        assert_eq!(text, sample.markdown);
        assert_eq!(stream.into_result().1, sample);
    }
}
