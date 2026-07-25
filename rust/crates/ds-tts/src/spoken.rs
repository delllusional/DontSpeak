//! Agent text → prose for the speech frontend.
//! Keep labels/code; drop formatting, HTML, link destinations, commit-like hashes.
//! Bare URLs → "link".

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

/// Prose safe for normalize + G2P.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpokenText(String);

/// GFM extensions — bare CommonMark would leave them as literal expander fodder.
fn markdown_options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_GFM
}

impl SpokenText {
    pub fn from_markdown(source: &str) -> Self {
        let mut rendered = String::with_capacity(source.len());

        for event in Parser::new_ext(source, markdown_options()) {
            match event {
                Event::Text(text) | Event::Code(text) => rendered.push_str(&text),
                Event::SoftBreak | Event::HardBreak | Event::Rule => rendered.push(' '),
                // Space both ends: nested `Start(List)` has no intervening `End`.
                Event::Start(tag) if is_block_start(&tag) => rendered.push(' '),
                Event::End(tag) if is_block_end(&tag) => rendered.push(' '),
                Event::Html(html) | Event::InlineHtml(html) => {
                    rendered.push_str(&strip_html_tags(&html));
                }
                _ => {}
            }
        }

        Self(omit_urls_and_collapse_whitespace(&rendered))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

fn is_block_start(tag: &Tag) -> bool {
    matches!(
        tag,
        Tag::Paragraph
            | Tag::Heading { .. }
            | Tag::BlockQuote(_)
            | Tag::CodeBlock(_)
            | Tag::HtmlBlock
            | Tag::List(_)
            | Tag::Item
            | Tag::FootnoteDefinition(_)
            | Tag::Table(_)
            | Tag::TableHead
            | Tag::TableRow
            | Tag::TableCell
    )
}

fn is_block_end(tag: &TagEnd) -> bool {
    matches!(
        tag,
        TagEnd::Paragraph
            | TagEnd::Heading(_)
            | TagEnd::BlockQuote(_)
            | TagEnd::CodeBlock
            | TagEnd::HtmlBlock
            | TagEnd::List(_)
            | TagEnd::Item
            | TagEnd::FootnoteDefinition
            | TagEnd::Table
            | TagEnd::TableHead
            | TagEnd::TableRow
            | TagEnd::TableCell
    )
}

/// Drop tags/comments; keep residual text. Unterminated `<` swallows the rest.
fn strip_html_tags(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(open) = rest.find('<') {
        text.push_str(&rest[..open]);
        let tail = &rest[open..];
        let consumed = if let Some(comment) = tail.strip_prefix("<!--") {
            comment.find("-->").map(|end| open + 4 + end + 3)
        } else {
            tail.find('>').map(|end| open + end + 1)
        };
        match consumed {
            Some(end) => rest = &rest[end..],
            None => return text,
        }
    }
    text.push_str(rest);
    text
}

fn omit_urls_and_collapse_whitespace(rendered: &str) -> String {
    let mut spoken = String::with_capacity(rendered.len());
    for token in rendered.split_whitespace().map(spoken_token) {
        match token {
            SpokenToken::Omitted => {}
            SpokenToken::Attached(punctuation) => spoken.push_str(&punctuation),
            SpokenToken::Separated(word) => {
                if !spoken.is_empty() {
                    spoken.push(' ');
                }
                spoken.push_str(&word);
            }
        }
    }
    spoken
}

/// URLs → "link" (wrappers kept). Omitted hashes attach trailing prosody punctuation.
fn spoken_token(token: &str) -> SpokenToken {
    const OPENERS: &[char] = &['(', '[', '{', '<', '"', '\'', '‘', '“', '„', '«', '‹'];
    // '“' is EN opener and DE closer — at token end only the closer applies.
    const CLOSERS: &[char] = &[
        ')', ']', '}', '>', '"', '\'', '’', '”', '“', '»', '›', '.', ',', ';', ':', '!', '?', '…',
        '。', '，', '；', '：', '！', '？',
    ];
    const SPOKEN_PUNCTUATION: &[char] = &[
        '.', ',', ';', ':', '!', '?', '…', '。', '，', '；', '：', '！', '？',
    ];
    // Split at first unicode sentence boundary (may appear mid-URL).
    const UNICODE_BOUNDARIES: &[char] =
        &['’', '”', '»', '›', '…', '。', '，', '；', '：', '！', '？'];

    let after_open = token.trim_start_matches(OPENERS);
    let leading = &token[..token.len() - after_open.len()];
    let boundary = after_open
        .char_indices()
        .find_map(|(index, ch)| UNICODE_BOUNDARIES.contains(&ch).then_some(index))
        .unwrap_or(after_open.len());
    let url = after_open[..boundary].trim_end_matches(CLOSERS);
    let trailing = &after_open[url.len()..];

    if is_hash_like(token) {
        let core_end = token
            .trim_end_matches(|ch: char| !ch.is_ascii_alphanumeric())
            .len();
        let punctuation: String = token[core_end..]
            .chars()
            .filter(|ch| SPOKEN_PUNCTUATION.contains(ch))
            .collect();
        if punctuation.is_empty() {
            SpokenToken::Omitted
        } else {
            SpokenToken::Attached(punctuation)
        }
    } else if url.starts_with("https://") || url.starts_with("http://") || url.starts_with("www.") {
        SpokenToken::Separated(format!("{leading}link{trailing}"))
    } else {
        SpokenToken::Separated(token.to_string())
    }
}

enum SpokenToken {
    Omitted,
    Attached(String),
    Separated(String),
}

/// Hex 7–40 chars with both a digit and an a–f letter (all-letter hex is English words).
fn is_hash_like(token: &str) -> bool {
    let core = token.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    let len = core.len();
    (7..=40).contains(&len)
        && core.chars().all(|ch| ch.is_ascii_hexdigit())
        && core.chars().any(|ch| ch.is_ascii_alphabetic())
        && core.chars().any(|ch| ch.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::SpokenText;

    #[test]
    fn renders_commonmark_as_prose_without_link_targets() {
        let spoken = SpokenText::from_markdown(
            "## Result\n\nUse **shared** `phonemes`; see [the audit](https://example.com/a).",
        );
        assert_eq!(
            spoken.as_str(),
            "Result Use shared phonemes; see the audit."
        );
    }

    #[test]
    fn drops_hashes_without_mangling_identifiers_paths_or_anchors() {
        assert_eq!(
            SpokenText::from_markdown(
                "Edit foo_bar in src/main at eedfc57; see src/pipeline.rs#shared-english-frontend."
            )
            .as_str(),
            "Edit foo_bar in src/main at; see src/pipeline.rs#shared-english-frontend."
        );
        assert_eq!(
            SpokenText::from_markdown("Line 1234567 of 2026 tests pass.").as_str(),
            "Line 1234567 of 2026 tests pass."
        );
    }

    #[test]
    fn dropped_hashes_preserve_trailing_punctuation_without_added_spaces() {
        assert_eq!(
            SpokenText::from_markdown("Fixed in eedfc57. Then run the tool.").as_str(),
            "Fixed in. Then run the tool."
        );
        assert_eq!(
            SpokenText::from_markdown("eedfc57. Then run the tool.").as_str(),
            ". Then run the tool."
        );
        assert_eq!(
            SpokenText::from_markdown("Use (eedfc57), then stop.").as_str(),
            "Use, then stop."
        );
    }

    /// All-letter a–f words are not hashes; hashes always carry a digit.
    #[test]
    fn hex_letter_words_are_spoken_not_dropped() {
        assert_eq!(
            SpokenText::from_markdown("The banner was defaced, then effaced; they acceded.")
                .as_str(),
            "The banner was defaced, then effaced; they acceded."
        );
        assert_eq!(
            SpokenText::from_markdown("See eedfc5710bd3c0a5c4a1f2e6d7b8093c4e5f6a71 for context.")
                .as_str(),
            "See for context."
        );
    }

    #[test]
    fn omits_bare_urls_but_keeps_surrounding_words() {
        let spoken =
            SpokenText::from_markdown("Read the report at https://example.com/a?q=1 and continue.");
        assert_eq!(spoken.as_str(), "Read the report at link and continue.");
    }

    #[test]
    fn keeps_code_block_content_without_fences_or_language_tag() {
        let spoken = SpokenText::from_markdown("```rust\ncargo test -p ds-tts\n```");
        assert_eq!(spoken.as_str(), "cargo test -p ds-tts");
    }

    /// Nested `Start(List)` has no intervening `End` — End-only spacing glued "outerinner".
    #[test]
    fn nested_list_items_do_not_glue_into_one_word() {
        let spoken = SpokenText::from_markdown("- outer\n  - inner one\n- outer two");
        assert_eq!(spoken.as_str(), "outer inner one outer two");

        let deep = SpokenText::from_markdown("- a\n  - b\n    - c\n- d");
        assert_eq!(deep.as_str(), "a b c d");

        let ordered = SpokenText::from_markdown("1. first\n   1. sub a\n2. second");
        assert_eq!(ordered.as_str(), "first sub a second");
    }

    /// GFM tables: cells as prose (else pipes/`---` spoken).
    #[test]
    fn table_cells_are_spoken_as_separated_prose() {
        let spoken = SpokenText::from_markdown("| Stage | Result |\n|---|---|\n| G2P | ok |");
        assert_eq!(spoken.as_str(), "Stage Result G2P ok");
    }

    /// Footnotes: marker silent; definition spoken (else "Claim caret one").
    #[test]
    fn footnote_markers_are_not_spoken_but_the_definition_is() {
        let spoken = SpokenText::from_markdown("Claim[^1].\n\n[^1]: The source.");
        assert_eq!(spoken.as_str(), "Claim. The source.");
    }

    /// Tasklists + strike: drop markers/syntax, keep text.
    #[test]
    fn task_markers_and_strikethrough_syntax_are_dropped() {
        assert_eq!(
            SpokenText::from_markdown("- [x] shipped\n- [ ] pending").as_str(),
            "shipped pending"
        );
        assert_eq!(
            SpokenText::from_markdown("This is ~~gone~~ removed.").as_str(),
            "This is gone removed."
        );
    }

    #[test]
    fn github_alert_marker_is_structure_not_prose() {
        let spoken = SpokenText::from_markdown("> [!NOTE]\n> Read the migration note.");
        assert_eq!(spoken.as_str(), "Read the migration note.");
    }

    /// Regression: replacing the whole token dropped the sentence's final stop, fusing
    /// sentences and removing the batcher's preferred break point.
    #[test]
    fn url_replacement_preserves_surrounding_punctuation() {
        assert_eq!(
            SpokenText::from_markdown("The docs are at https://example.com. Then run the tool.")
                .as_str(),
            "The docs are at link. Then run the tool."
        );
        assert_eq!(
            SpokenText::from_markdown("Did you read https://example.com?").as_str(),
            "Did you read link?"
        );
        assert_eq!(
            SpokenText::from_markdown("Open (see https://example.com) next.").as_str(),
            "Open (see link) next."
        );
        assert_eq!(
            SpokenText::from_markdown("Open “https://example.com”… Then continue.").as_str(),
            "Open “link”… Then continue."
        );
        assert_eq!(
            SpokenText::from_markdown("参照 https://example.com。次へ。").as_str(),
            "参照 link。次へ。"
        );
        // German-style quotes: '“' closes what '„' opened and must survive the replacement.
        assert_eq!(
            SpokenText::from_markdown("Siehe „https://example.com“ hier.").as_str(),
            "Siehe „link“ hier."
        );
    }

    #[test]
    fn degenerate_input_renders_empty_without_panicking() {
        for source in ["", "   ", "---", "<div><span></span></div>"] {
            assert_eq!(SpokenText::from_markdown(source).as_str(), "");
        }
    }

    /// Regression (audit): a block-level HTML element's visible prose was dropped wholesale —
    /// only its tags are structure. `<details>` blocks are common in agent replies.
    #[test]
    fn html_block_keeps_visible_text_and_drops_tags() {
        assert_eq!(
            SpokenText::from_markdown("<div>Hello</div>").as_str(),
            "Hello"
        );
        assert_eq!(
            SpokenText::from_markdown(
                "<details><summary>Notes</summary>\n\nBody text.\n\n</details>"
            )
            .as_str(),
            "Notes Body text."
        );
        assert_eq!(
            SpokenText::from_markdown("Before <!-- hidden --> after.").as_str(),
            "Before after."
        );
    }
}
