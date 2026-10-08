//! A canned briefing for `--demo` and tests, so the panel can be tried
//! without a network or an API key.

use super::{Insight, Kind, Subject};

/// A plausible briefing for `subject`, plainly marked as a sample.
pub fn insight(subject: &Subject) -> Insight {
    let artist = subject.main_artist().unwrap_or("the artist");
    let name = &subject.name;
    let markdown = match subject.kind {
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
    };
    Insight {
        markdown,
        created_at: jiff::Timestamp::now().as_second(),
        truncated: false,
        cached: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_has_a_sample_that_parses() {
        for uri in ["spotify:album:a", "spotify:artist:a", "spotify:track:a"] {
            let subject = Subject::from_uri(uri, "Fragments").unwrap();
            let insight = insight(&subject);
            assert!(insight.markdown.contains("Fragments"));
            let blocks = super::super::markdown::parse(&insight.markdown);
            assert!(blocks.iter().any(|block| matches!(
                block,
                super::super::markdown::Block::Heading { level: 2, .. }
            )));
        }
    }
}
