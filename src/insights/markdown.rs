//! The small part of Markdown a briefing uses: headings, bullets, numbered
//! items, paragraphs, rules, bold, italics, inline code and links.
//!
//! Parsing is separate from drawing so it can be tested; the panel in
//! `src/ui/insights.rs` draws the blocks.

/// A run of text in one style.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Span {
    Text {
        text: String,
        bold: bool,
        italic: bool,
    },
    Code(String),
    Link {
        text: String,
        url: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Heading {
        level: u8,
        spans: Vec<Span>,
    },
    Paragraph(Vec<Span>),
    /// A bullet, `depth` levels in.
    Bullet {
        depth: u8,
        spans: Vec<Span>,
    },
    Numbered {
        number: u32,
        spans: Vec<Span>,
    },
    Quote(Vec<Span>),
    Rule,
}

/// The blocks of `text`.
pub fn parse(text: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut paragraph: Vec<String> = Vec::new();
    let flush = |paragraph: &mut Vec<String>, blocks: &mut Vec<Block>| {
        if !paragraph.is_empty() {
            blocks.push(Block::Paragraph(inline(&paragraph.join(" "))));
            paragraph.clear();
        }
    };
    let mut in_fence = false;
    for raw in text.lines() {
        let indent = raw.len() - raw.trim_start().len();
        let line = raw.trim();
        if line.starts_with("```") {
            // Code fences are not expected; their lines read as text.
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            if !line.is_empty() {
                paragraph.push(line.to_string());
            }
            continue;
        }
        if line.is_empty() {
            flush(&mut paragraph, &mut blocks);
            continue;
        }
        if is_rule(line) {
            flush(&mut paragraph, &mut blocks);
            blocks.push(Block::Rule);
            continue;
        }
        if let Some((level, rest)) = heading(line) {
            flush(&mut paragraph, &mut blocks);
            blocks.push(Block::Heading {
                level,
                spans: inline(rest),
            });
            continue;
        }
        if let Some(rest) = bullet(line) {
            flush(&mut paragraph, &mut blocks);
            blocks.push(Block::Bullet {
                depth: (indent / 2).min(3) as u8,
                spans: inline(rest),
            });
            continue;
        }
        if let Some((number, rest)) = numbered(line) {
            flush(&mut paragraph, &mut blocks);
            blocks.push(Block::Numbered {
                number,
                spans: inline(rest),
            });
            continue;
        }
        if let Some(rest) = line.strip_prefix('>') {
            flush(&mut paragraph, &mut blocks);
            blocks.push(Block::Quote(inline(rest.trim())));
            continue;
        }
        // A wrapped bullet continues on an indented line.
        if indent > 0
            && paragraph.is_empty()
            && let Some(Block::Bullet { spans, .. } | Block::Numbered { spans, .. }) =
                blocks.last_mut()
        {
            spans.push(plain(" "));
            spans.extend(inline(line));
            merge(spans);
            continue;
        }
        paragraph.push(line.to_string());
    }
    flush(&mut paragraph, &mut blocks);
    blocks
}

fn is_rule(line: &str) -> bool {
    let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    compact.len() >= 3
        && (compact.chars().all(|c| c == '-')
            || compact.chars().all(|c| c == '*')
            || compact.chars().all(|c| c == '_'))
}

fn heading(line: &str) -> Option<(u8, &str)> {
    let level = line.chars().take_while(|c| *c == '#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let rest = &line[level..];
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    Some((level as u8, rest.trim().trim_end_matches('#').trim_end()))
}

fn bullet(line: &str) -> Option<&str> {
    ["- ", "* ", "+ ", "\u{2022} "]
        .iter()
        .find_map(|marker| line.strip_prefix(marker))
        .map(str::trim)
}

fn numbered(line: &str) -> Option<(u32, &str)> {
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 || digits > 3 {
        return None;
    }
    let rest = &line[digits..];
    let rest = rest
        .strip_prefix(". ")
        .or_else(|| rest.strip_prefix(") "))?;
    Some((line[..digits].parse().ok()?, rest.trim()))
}

/// Joins neighbouring runs of the same style.
fn merge(spans: &mut Vec<Span>) {
    let mut merged: Vec<Span> = Vec::with_capacity(spans.len());
    for span in spans.drain(..) {
        if let (
            Some(Span::Text {
                text: held,
                bold: held_bold,
                italic: held_italic,
            }),
            Span::Text { text, bold, italic },
        ) = (merged.last_mut(), &span)
            && held_bold == bold
            && held_italic == italic
        {
            held.push_str(text);
            continue;
        }
        merged.push(span);
    }
    *spans = merged;
}

fn plain(text: &str) -> Span {
    Span::Text {
        text: text.to_string(),
        bold: false,
        italic: false,
    }
}

/// The styled runs of one line or paragraph.
pub fn inline(text: &str) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    let mut current = String::new();
    let mut bold = false;
    let mut italic = false;
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let push_text = |spans: &mut Vec<Span>, current: &mut String, bold: bool, italic: bool| {
        if current.is_empty() {
            return;
        }
        // Merge with the previous run when the style is the same.
        if let Some(Span::Text {
            text,
            bold: b,
            italic: it,
        }) = spans.last_mut()
            && *b == bold
            && *it == italic
        {
            text.push_str(current);
        } else {
            spans.push(Span::Text {
                text: std::mem::take(current),
                bold,
                italic,
            });
        }
        current.clear();
    };
    while i < chars.len() {
        let c = chars[i];
        // Escaped characters stand for themselves.
        if c == '\\' && i + 1 < chars.len() && chars[i + 1].is_ascii_punctuation() {
            current.push(chars[i + 1]);
            i += 2;
            continue;
        }
        if c == '`'
            && let Some(end) = find(&chars, i + 1, &['`'])
        {
            push_text(&mut spans, &mut current, bold, italic);
            spans.push(Span::Code(chars[i + 1..end].iter().collect()));
            i = end + 1;
            continue;
        }
        if c == '['
            && let Some((label, url, next)) = link(&chars, i)
        {
            push_text(&mut spans, &mut current, bold, italic);
            spans.push(Span::Link { text: label, url });
            i = next;
            continue;
        }
        if (c == '*' || c == '_') && i + 1 < chars.len() && chars[i + 1] == c {
            // A bold marker only opens before text and closes after it.
            let opens = !bold && chars.get(i + 2).is_some_and(|n| !n.is_whitespace());
            let closes = bold && i > 0 && !chars[i - 1].is_whitespace();
            if opens && find_pair(&chars, i + 2, c) || closes {
                push_text(&mut spans, &mut current, bold, italic);
                bold = !bold;
            } else {
                current.push(c);
                current.push(c);
            }
            i += 2;
            continue;
        }
        if c == '*' || c == '_' {
            let opens = !italic
                && chars.get(i + 1).is_some_and(|n| !n.is_whitespace())
                // Snake_case and 2*3 are not emphasis.
                && (i == 0 || !chars[i - 1].is_alphanumeric())
                && find(&chars, i + 1, &[c]).is_some();
            let closes = italic
                && i > 0
                && !chars[i - 1].is_whitespace()
                && chars.get(i + 1).is_none_or(|n| !n.is_alphanumeric());
            if opens || closes {
                push_text(&mut spans, &mut current, bold, italic);
                italic = !italic;
                i += 1;
                continue;
            }
        }
        if (text_at(&chars, i, "https://") || text_at(&chars, i, "http://"))
            && (i == 0 || !chars[i - 1].is_alphanumeric())
        {
            let end = (i..chars.len())
                .find(|&j| chars[j].is_whitespace())
                .unwrap_or(chars.len());
            let mut url: String = chars[i..end].iter().collect();
            // Sentence punctuation after a bare link is not part of it, nor
            // is a closing parenthesis it did not open; Wikipedia's
            // "Purple_Rain_(album)" keeps its own.
            loop {
                let unbalanced =
                    url.ends_with(')') && url.matches(')').count() > url.matches('(').count();
                if url.ends_with(['.', ',', ';', ':', '!', '?']) || unbalanced {
                    url.pop();
                } else {
                    break;
                }
            }
            let taken = url.chars().count();
            push_text(&mut spans, &mut current, bold, italic);
            spans.push(Span::Link {
                text: url.clone(),
                url,
            });
            i += taken;
            continue;
        }
        current.push(c);
        i += 1;
    }
    push_text(&mut spans, &mut current, bold, italic);
    spans
}

fn text_at(chars: &[char], at: usize, wanted: &str) -> bool {
    wanted
        .chars()
        .enumerate()
        .all(|(offset, c)| chars.get(at + offset) == Some(&c))
}

fn find(chars: &[char], from: usize, any: &[char]) -> Option<usize> {
    (from..chars.len()).find(|&j| any.contains(&chars[j]))
}

/// Whether a closing `cc` follows `from`.
fn find_pair(chars: &[char], from: usize, c: char) -> bool {
    (from..chars.len().saturating_sub(1)).any(|j| chars[j] == c && chars[j + 1] == c)
}

/// `[label](url)` at `start`: the label, the URL and where reading resumes.
fn link(chars: &[char], start: usize) -> Option<(String, String, usize)> {
    let close = find(chars, start + 1, &[']'])?;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    // The URL ends at the parenthesis that closes it, so one with its own
    // parentheses, as Wikipedia's often have, stays whole.
    let mut depth = 0usize;
    let end = (close + 2..chars.len()).find(|&j| match chars[j] {
        '(' => {
            depth += 1;
            false
        }
        ')' if depth == 0 => true,
        ')' => {
            depth -= 1;
            false
        }
        _ => false,
    })?;
    let url: String = chars[close + 2..end].iter().collect();
    let url = url.trim().to_string();
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return None;
    }
    let label: String = chars[start + 1..close].iter().collect();
    let label = label.replace("**", "").replace('`', "");
    Some((label, url, end + 1))
}

/// The plain text of `spans`, for screen readers and tests.
pub fn plain_text(spans: &[Span]) -> String {
    spans
        .iter()
        .map(|span| match span {
            Span::Text { text, .. } | Span::Code(text) | Span::Link { text, .. } => text.as_str(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(text: &str, bold: bool, italic: bool) -> Span {
        Span::Text {
            text: text.into(),
            bold,
            italic,
        }
    }

    #[test]
    fn blocks_are_recognised() {
        let blocks = parse(
            "Intro line one\nline two\n\n## Context\n- **Prince** produced it\n  - nested\n1. First\n2) Second\n> quoted\n---\n# Big",
        );
        assert_eq!(
            blocks,
            [
                Block::Paragraph(vec![text("Intro line one line two", false, false)]),
                Block::Heading {
                    level: 2,
                    spans: vec![text("Context", false, false)]
                },
                Block::Bullet {
                    depth: 0,
                    spans: vec![
                        text("Prince", true, false),
                        text(" produced it", false, false)
                    ]
                },
                Block::Bullet {
                    depth: 1,
                    spans: vec![text("nested", false, false)]
                },
                Block::Numbered {
                    number: 1,
                    spans: vec![text("First", false, false)]
                },
                Block::Numbered {
                    number: 2,
                    spans: vec![text("Second", false, false)]
                },
                Block::Quote(vec![text("quoted", false, false)]),
                Block::Rule,
                Block::Heading {
                    level: 1,
                    spans: vec![text("Big", false, false)]
                },
            ]
        );
    }

    #[test]
    fn hashes_without_a_space_are_not_headings() {
        assert_eq!(
            parse("#1 hit"),
            [Block::Paragraph(vec![text("#1 hit", false, false)])]
        );
        assert_eq!(
            parse("1999 was a year"),
            [Block::Paragraph(vec![text(
                "1999 was a year",
                false,
                false
            )])]
        );
    }

    #[test]
    fn wrapped_bullets_continue_on_indented_lines() {
        assert_eq!(
            parse("- first part\n  second part"),
            [Block::Bullet {
                depth: 0,
                spans: vec![text("first part second part", false, false)]
            }]
        );
    }

    #[test]
    fn inline_styles_and_links() {
        assert_eq!(
            inline(
                "A **bold** and *italic* with `code` and [Wikipedia](https://en.wikipedia.org/wiki/X)."
            ),
            [
                text("A ", false, false),
                text("bold", true, false),
                text(" and ", false, false),
                text("italic", false, true),
                text(" with ", false, false),
                Span::Code("code".into()),
                text(" and ", false, false),
                Span::Link {
                    text: "Wikipedia".into(),
                    url: "https://en.wikipedia.org/wiki/X".into()
                },
                text(".", false, false),
            ]
        );
    }

    #[test]
    fn stray_markers_stay_as_text() {
        assert_eq!(inline("2 * 3 = 6"), [text("2 * 3 = 6", false, false)]);
        assert_eq!(
            inline("snake_case_name"),
            [text("snake_case_name", false, false)]
        );
        assert_eq!(inline("a ** b"), [text("a ** b", false, false)]);
        assert_eq!(
            inline("[not a link] (x)"),
            [text("[not a link] (x)", false, false)]
        );
        assert_eq!(inline("\\*literal\\*"), [text("*literal*", false, false)]);
    }

    #[test]
    fn only_web_links_are_links() {
        assert_eq!(
            inline("[x](javascript:alert(1))"),
            [text("[x](javascript:alert(1))", false, false)]
        );
    }

    #[test]
    fn bare_urls_become_links_without_trailing_punctuation() {
        assert_eq!(
            inline("See https://musicbrainz.org/release/1."),
            [
                text("See ", false, false),
                Span::Link {
                    text: "https://musicbrainz.org/release/1".into(),
                    url: "https://musicbrainz.org/release/1".into()
                },
                text(".", false, false),
            ]
        );
    }

    #[test]
    fn urls_keep_their_own_parentheses() {
        let wiki = "https://en.wikipedia.org/wiki/Purple_Rain_(album)";
        assert_eq!(
            inline(&format!("[Purple Rain]({wiki}) was next.")),
            [
                Span::Link {
                    text: "Purple Rain".into(),
                    url: wiki.into()
                },
                text(" was next.", false, false),
            ]
        );
        assert_eq!(
            inline(&format!("(see {wiki}).")),
            [
                text("(see ", false, false),
                Span::Link {
                    text: wiki.into(),
                    url: wiki.into()
                },
                text(").", false, false),
            ]
        );
    }

    #[test]
    fn odd_input_never_panics() {
        for sample in [
            "",
            "#",
            "######## x",
            "- ",
            "1.",
            "[",
            "[]",
            "[](",
            "[x](https://",
            "**",
            "*",
            "`",
            "\\",
            "https://",
            "> ",
            "  ",
            "\u{301}",
            "## \u{1f3b5}**\u{e9}",
            "[\u{e9}](https://\u{e9}(\u{e9})",
            "_\u{1f600}_",
            "1) \u{65e5}\u{672c}",
            "***bold italic***",
        ] {
            let _ = parse(sample);
        }
    }

    #[test]
    fn every_partial_answer_parses() {
        // While an answer streams in, the panel parses whatever has arrived,
        // which can stop anywhere: mid-link, mid-bold, mid-character.
        let answer = "Intro with **bold** and *it\u{e9}*.\n\n## Who \u{1f3b8}\n\
            - **Produced by** [Prince](https://example.org/a_(b)) on `x`\n\
            1. Opening track, https://example.org/x.\n> Quote\n---\n";
        for (at, _) in answer.char_indices() {
            let _ = parse(&answer[..at]);
        }
        assert!(!parse(answer).is_empty());
    }

    #[test]
    fn plain_text_drops_the_styling() {
        let spans = inline("**Prince** on [guitar](https://x.org)");
        assert_eq!(plain_text(&spans), "Prince on guitar");
    }
}
