//! Render agent text into the prose that the speech frontend should receive.
//!
//! Markdown is structure, not pronunciation. Parsing it preserves visible labels and code
//! while omitting formatting syntax, HTML tags, and link destinations. Bare URL tokens become
//! the word "link" because reading them character-by-character is rarely useful narration.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

/// Prose safe to pass to text normalization and grapheme-to-phoneme conversion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpokenText(String);

/// Agent replies are GitHub-flavored, not bare CommonMark. `Parser::new` enables NO
/// extensions, so tables, `~~strike~~`, `- [x]` and `[^1]` survive as literal text and reach
/// the number expander — a footnote marker came out as "Claim caret one" and a table was read
/// as one run-on line of pipes and dashes. Enable exactly the constructs we then discard.
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
                // LOAD-BEARING: a block boundary is a word boundary at BOTH ends. pulldown-cmark
                // emits `Start(List)` for a NESTED list immediately after the parent item's inline
                // text with no intervening `End`, so separating only on `End` glued
                // "- outer\n  - inner" into the single OOV word "outerinner".
                Event::Start(tag) if is_block_start(&tag) => rendered.push(' '),
                Event::End(tag) if is_block_end(&tag) => rendered.push(' '),
                // Raw HTML keeps its visible text, not its tags: a block-level element
                // (`<div>Hello</div>`, `<details><summary>Notes</summary>`) arrives as one
                // `Html` event whose inner prose would otherwise be silenced wholesale.
                Event::Html(html) | Event::InlineHtml(html) => {
                    rendered.push_str(&strip_html_tags(&html));
                }
                // Footnote markers and checkbox glyphs are structure, not prose.
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

/// Drop `<...>` tag spans and `<!-- ... -->` comments from raw HTML, keeping the residual
/// visible text. Speech-oriented, not an HTML parser: an unterminated `<` swallows the rest
/// of the event, which for well-formed agent output only ever drops markup.
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
    rendered
        .split_whitespace()
        .map(spoken_token)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Replace a bare URL with the word "link", **keeping the punctuation wrapped around it**.
/// Replacing the whole whitespace token swallowed the sentence's final stop
/// ("See https://example.com." → "See link"), which fused two sentences into one run-on and
/// destroyed the `.!?` boundaries `batch::pack_batches` prefers to break on.
fn spoken_token(token: &str) -> String {
    const OPENERS: &[char] = &['(', '[', '{', '<', '"', '\'', '‘', '“', '„', '«', '‹'];
    // '“' is both an English OPENER and the German-style CLOSER („so“) — a token end is
    // only ever the closing role, so listing it here is safe.
    const CLOSERS: &[char] = &[
        ')', ']', '}', '>', '"', '\'', '’', '”', '“', '»', '›', '.', ',', ';', ':', '!', '?', '…',
        '。', '，', '；', '：', '！', '？',
    ];
    // In agent prose these commonly delimit the next sentence without whitespace. Raw URL
    // path/query text can contain them, so this is intentionally a speech-oriented heuristic,
    // not a general URL parser. Split at the first one instead of only trimming the token end.
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

    if url.starts_with("https://") || url.starts_with("http://") || url.starts_with("www.") {
        format!("{leading}link{trailing}")
    } else {
        token.to_string()
    }
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

    /// Regression: `Start(List)` follows the parent item's text with no `End` between, so
    /// separating only on `End` produced "outerinner one" — one unpronounceable OOV word.
    /// Nested bullets are pervasive in agent replies.
    #[test]
    fn nested_list_items_do_not_glue_into_one_word() {
        let spoken = SpokenText::from_markdown("- outer\n  - inner one\n- outer two");
        assert_eq!(spoken.as_str(), "outer inner one outer two");

        let deep = SpokenText::from_markdown("- a\n  - b\n    - c\n- d");
        assert_eq!(deep.as_str(), "a b c d");

        let ordered = SpokenText::from_markdown("1. first\n   1. sub a\n2. second");
        assert_eq!(ordered.as_str(), "first sub a second");
    }

    /// Regression: without `ENABLE_TABLES` the pipes and the `---` rule survived as text and
    /// were read aloud; `---` also became an em-dash and the cells were number-expanded.
    #[test]
    fn table_cells_are_spoken_as_separated_prose() {
        let spoken = SpokenText::from_markdown("| Stage | Result |\n|---|---|\n| G2P | ok |");
        assert_eq!(spoken.as_str(), "Stage Result G2P ok");
    }

    /// Regression: without `ENABLE_FOOTNOTES` the marker stayed literal and `expand_numbers`
    /// later read `Claim[^1].` as "Claim caret one."
    #[test]
    fn footnote_markers_are_not_spoken_but_the_definition_is() {
        let spoken = SpokenText::from_markdown("Claim[^1].\n\n[^1]: The source.");
        assert_eq!(spoken.as_str(), "Claim. The source.");
    }

    /// Regression: without the extensions these read as "[x] shipped" and "~~gone~~".
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
