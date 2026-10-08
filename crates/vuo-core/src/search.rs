//! Searching the local mirror.
//!
//! §5: the mirror is the UI's single source of truth and the UI never waits on
//! the network, so search runs against what has been synced rather than asking
//! the server. It works offline, and it finds exactly what the lists can show.
//!
//! # What matches
//!
//! A query is split on whitespace into terms, and an article matches when
//! EVERY term appears somewhere in its title or the TEXT of its body. Case is
//! ignored. A term is a plain substring: no wildcards, no operators, so `50%`
//! and `C++` mean what they say.
//!
//! The author is not searched. A list row does not show it, so an article
//! found by its author would be a result with nothing on screen to say why.
//!
//! Nor is the name of an article's feed. A feed's name finds the FEED, which
//! matches when every term is in its name ([`SearchQuery::matches`]): a
//! reader typing a feed's name is after that feed, not after its latest
//! articles, which were all the name used to find.
//!
//! # Where it matched
//!
//! Results are grouped by WHERE they matched -- the [`MatchKind`] -- so the
//! articles a reader is most likely after, the ones whose title says so, come
//! first. For an article the kind is title when its title holds any term, and
//! body text otherwise; the feeds are a group of their own. For the result
//! rows themselves, [`SearchQuery::highlight`] marks the terms in a title or
//! feed name, and [`SearchQuery::excerpt`] cuts the lines around a match out
//! of a body.
//!
//! "The text of its body" is the point of this module. Bodies are stored as
//! the HTML Miniflux delivered, and a substring test over the raw markup would
//! match every article for `div`, `href` or `class`. It would also miss text
//! the markup escapes: Miniflux delivers UTF-8 but escapes `&`, `<` and `>`,
//! so `AT&T` is stored as `AT&amp;T` and a raw search for it finds nothing.
//! [`html_text`] strips the markup and decodes the character references that
//! matter before anything is compared.
//!
//! # Why a SQL function
//!
//! The matching runs inside SQLite as `vuo_search_kind(...)`, registered on
//! every connection by [`crate::db::Database`]. The alternative -- reading
//! every body out to filter it in Rust -- is the shape `store::EntryListRow`
//! exists to avoid: it carried every body in the mirror through memory at
//! once. Inside the query each body is looked at once, in SQLite's own buffer,
//! and dropped before the next row.

use rusqlite::functions::{Context, FunctionFlags};
use rusqlite::Connection;

/// The SQL name the matcher is registered under. `store::search_entries`
/// spells it out in its statement, which is a literal by rule (§9.4).
const SQL_FUNCTION: &str = "vuo_search_kind";

/// Where a search result matched, which is the group it is listed in. In the
/// order the groups are listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MatchKind {
    /// An article whose title holds a term.
    Title = 1,
    /// A FEED, whose name holds every term -- see [`SearchQuery::matches`].
    /// Never an article: [`SearchQuery::classify`] does not return it.
    Feed = 2,
    /// An article whose body's text holds a term, and whose title does not.
    Text = 3,
}

impl MatchKind {
    /// The kind as SQL hands it back: 1 or 3 for an article, as
    /// [`SearchQuery::classify`] does; anything else is no match.
    #[must_use]
    pub fn from_sql(value: i64) -> Option<MatchKind> {
        match value {
            1 => Some(MatchKind::Title),
            3 => Some(MatchKind::Text),
            _ => None,
        }
    }
}

/// How much of a body an excerpt shows before the match, in characters:
/// about half a line on a phone, so the match lands on the excerpt's first
/// line with what leads up to it.
const EXCERPT_LEAD: usize = 40;
/// How long an excerpt is at most, in characters. A little more than the
/// three lines the list shows, so the view rather than this decides where
/// the text is cut.
const EXCERPT_LENGTH: usize = 200;

/// A parsed query: its terms, case-folded. Never empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    terms: Vec<String>,
}

impl SearchQuery {
    /// Parse what the reader typed. `None` when there is nothing to search
    /// for, which callers treat as "no results" rather than "everything".
    #[must_use]
    pub fn parse(raw: &str) -> Option<SearchQuery> {
        let terms: Vec<String> = raw.split_whitespace().map(fold).collect();
        if terms.is_empty() {
            None
        } else {
            Some(SearchQuery { terms })
        }
    }

    /// The folded terms, in the order they were typed.
    #[must_use]
    pub fn terms(&self) -> &[String] {
        &self.terms
    }

    /// Whether every term is in `text`. What finds a feed by its name.
    #[must_use]
    pub fn matches(&self, text: &str) -> bool {
        let text = fold(text);
        self.terms.iter().all(|t| text.contains(t.as_str()))
    }

    /// Where an article matches -- [`MatchKind::Title`] or
    /// [`MatchKind::Text`] -- or `None` when it does not: every term must be
    /// somewhere in its title or the text of its body.
    ///
    /// The body is the expensive part, so it is stripped and folded only when
    /// the title has not already accounted for every term.
    #[must_use]
    pub fn classify(&self, title: &str, content_html: &str) -> Option<MatchKind> {
        let title = fold(title);
        let rest: Vec<&String> = self
            .terms
            .iter()
            .filter(|t| !title.contains(t.as_str()))
            .collect();
        if !rest.is_empty() {
            let mut body = html_text(content_html);
            if rest.iter().all(|t| t.is_ascii()) {
                // In place, and several times faster than `to_lowercase`. The
                // same answer for an ASCII term, but for a handful of exotic
                // code points that `to_lowercase` folds INTO ASCII (the Kelvin
                // sign becomes `k`), which no reader will miss.
                body.make_ascii_lowercase();
            } else {
                body = fold(&body);
            }
            if !rest.iter().all(|t| body.contains(t.as_str())) {
                return None;
            }
        }
        Some(if rest.len() < self.terms.len() {
            MatchKind::Title
        } else {
            MatchKind::Text
        })
    }

    /// `text` as Qt `StyledText`, with every occurrence of a term in bold.
    ///
    /// For a title or a feed name in a result row. The text is FOREIGN, so
    /// it is escaped -- the only markup in the result is the `<b>` added
    /// here, which is what lets QML render it as `StyledText` (§9.3).
    #[must_use]
    pub fn highlight(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len() + 16);
        let mut copied = 0;
        for (start, end) in self.spans(text) {
            crate::content::block::escape_into(
                text.get(copied..start).unwrap_or_default(),
                &mut out,
            );
            out.push_str("<b>");
            crate::content::block::escape_into(text.get(start..end).unwrap_or_default(), &mut out);
            out.push_str("</b>");
            copied = end;
        }
        crate::content::block::escape_into(text.get(copied..).unwrap_or_default(), &mut out);
        out
    }

    /// The text around the first match in a body, as plain text, or `None`
    /// when no term is in its text.
    ///
    /// A few words before the match and a few lines after it, cut at word
    /// boundaries, with an ellipsis where the body goes on. Whitespace is
    /// collapsed: a body's line breaks are its markup's, not a reader's.
    #[must_use]
    pub fn excerpt(&self, content_html: &str) -> Option<String> {
        let text = html_text(content_html)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let (first, _) = self.spans(&text).into_iter().next()?;

        // In characters from here on: a byte count would cut a line of
        // Cyrillic to half the length of one in English.
        let chars: Vec<(usize, char)> = text.char_indices().collect();
        let at = chars.iter().position(|&(i, _)| i == first)?;
        let mut from = at.saturating_sub(EXCERPT_LEAD);
        if from > 0 {
            // Forward to the start of a word, but never past the match.
            from = (from..at)
                .find(|&i| chars.get(i.wrapping_sub(1)).is_some_and(|&(_, c)| c == ' '))
                .unwrap_or(at);
        }
        let mut to = (from + EXCERPT_LENGTH).min(chars.len());
        if to < chars.len() {
            // Back to the end of a word, unless that loses the match.
            if let Some(space) = (at + 1..to)
                .rev()
                .find(|&i| chars.get(i).is_some_and(|&(_, c)| c == ' '))
            {
                to = space;
            }
        }
        let byte = |i: usize| chars.get(i).map_or(text.len(), |&(b, _)| b);
        let mut out = String::new();
        if from > 0 {
            out.push('\u{2026}');
        }
        out.push_str(text.get(byte(from)..byte(to))?.trim());
        if to < chars.len() {
            out.push('\u{2026}');
        }
        Some(out)
    }

    /// Where the terms are in `text`: byte ranges of the ORIGINAL text,
    /// sorted, with overlapping and touching ones merged.
    fn spans(&self, text: &str) -> Vec<(usize, usize)> {
        // The folded text is searched, and every folded byte maps back to the
        // character it came from. `to_lowercase` folds a character the same
        // number of bytes whether it does it alone or in a string -- its one
        // context rule, the Greek final sigma, swaps one two-byte sigma for
        // another -- so the map built a character at a time fits the string
        // folded whole. If it ever did not, nothing is marked: a missing bold
        // is better than a misplaced one.
        let folded = fold(text);
        let mut origin = Vec::with_capacity(folded.len() + 1);
        for (at, ch) in text.char_indices() {
            let width: usize = ch.to_lowercase().map(char::len_utf8).sum();
            origin.extend(std::iter::repeat_n(at, width));
        }
        origin.push(text.len());
        if origin.len() != folded.len() + 1 {
            return Vec::new();
        }

        let mut found: Vec<(usize, usize)> = Vec::new();
        for term in &self.terms {
            let mut from = 0;
            while let Some(hit) = folded.get(from..).and_then(|rest| rest.find(term.as_str())) {
                let start = from + hit;
                let end = start + term.len();
                // The END maps to the start of the character after the match:
                // a match that ends inside one character's folding (a dotted
                // capital I folds to two) stops short of it.
                if let (Some(&a), Some(&b)) = (origin.get(start), origin.get(end)) {
                    if a < b {
                        found.push((a, b));
                    }
                }
                from = start + 1;
                while !folded.is_char_boundary(from) {
                    from += 1;
                }
            }
        }
        found.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::with_capacity(found.len());
        for (start, end) in found {
            match merged.last_mut() {
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        merged
    }
}

/// Case-fold for comparison.
///
/// `to_lowercase` rather than ASCII folding, which is what SQLite's own `LIKE`
/// does: a German or Finnish reader typing `ä` must find `Ä`.
fn fold(s: &str) -> String {
    s.to_lowercase()
}

/// Inline elements: closing one of these does not end a word, so they are
/// removed without leaving a space behind. `<b>Wo</b>rd` is the word `Word`.
const INLINE: &[&str] = &[
    "a", "abbr", "b", "bdi", "bdo", "cite", "code", "data", "dfn", "em", "i", "kbd", "mark", "q",
    "s", "samp", "small", "span", "strike", "strong", "sub", "sup", "time", "u", "var", "wbr",
];

/// The text of an HTML fragment, for searching.
///
/// Not a parser, and not the content transform: nothing here is rendered, so
/// all it has to get right is which characters a reader would have seen.
/// Tags are removed (block-level ones leave a space, so two paragraphs do not
/// run together into one word), and the character references a sanitised body
/// actually contains are decoded. Anything it does not recognise passes
/// through as text, which at worst makes a term findable that should not be.
#[must_use]
pub fn html_text(html: &str) -> String {
    // Scanned as BYTES, which is what makes this cheap enough to run over
    // every body in the mirror on each search: `<`, `>`, `&` and `;` are
    // ASCII, and an ASCII byte in UTF-8 is always a whole character, so every
    // index found here is a character boundary and every slice below is one
    // `get` that cannot fail.
    let bytes = html.as_bytes();
    let mut out = String::with_capacity(html.len());
    let mut copied = 0;
    let mut at = 0;
    while let Some(found) = bytes
        .get(at..)
        .and_then(|b| b.iter().position(|&c| c == b'<' || c == b'&'))
    {
        let mark = at + found;
        out.push_str(html.get(copied..mark).unwrap_or_default());
        if bytes.get(mark) == Some(&b'<') {
            // An unterminated tag ends the text: what follows is not
            // something a reader would have seen.
            let Some(len) = bytes
                .get(mark + 1..)
                .and_then(|b| b.iter().position(|&c| c == b'>'))
            else {
                return out;
            };
            if !is_inline(html.get(mark + 1..mark + 1 + len).unwrap_or_default()) {
                out.push(' ');
            }
            at = mark + 1 + len + 1;
        } else {
            match decode_entity(html.get(mark + 1..).unwrap_or_default()) {
                Some((ch, used)) => {
                    out.push(ch);
                    at = mark + 1 + used;
                }
                None => {
                    out.push('&');
                    at = mark + 1;
                }
            }
        }
        copied = at;
    }
    out.push_str(html.get(copied..).unwrap_or_default());
    out
}

/// Whether a tag's text (between `<` and `>`) names an inline element.
///
/// Compared in place rather than by building the lowercased name: this runs
/// once per tag in every body in the mirror.
fn is_inline(tag: &str) -> bool {
    let tag = tag.strip_prefix('/').unwrap_or(tag);
    let end = tag
        .find(|c: char| !c.is_ascii_alphanumeric())
        .unwrap_or(tag.len());
    let name = tag.get(..end).unwrap_or_default();
    INLINE.iter().any(|n| n.eq_ignore_ascii_case(name))
}

/// Decode a character reference at the start of `s` -- the text after `&`.
///
/// Returns the character and how many bytes of `s` it used, including the
/// `;`. Only references that end in `;` within a short distance are decoded;
/// anything else is not a reference and is left as text.
fn decode_entity(s: &str) -> Option<(char, usize)> {
    const LONGEST: usize = 10;
    let semi = s.get(..=LONGEST).unwrap_or(s).find(';')?;
    let name = s.get(..semi)?;
    let ch = if let Some(num) = name.strip_prefix('#') {
        let code = match num.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => num.parse::<u32>().ok()?,
        };
        char::from_u32(code)?
    } else {
        match name {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            // A non-breaking space separates words like any other.
            "nbsp" => ' ',
            _ => return None,
        }
    };
    Some((ch, semi + 1))
}

/// Register [`SQL_FUNCTION`] on a connection.
///
/// `vuo_search_kind(query, title, content)` is the [`MatchKind`] as a number
/// when the article matches, 0 when it does not, and 0 for a
/// query with no terms. The query is parsed once per statement and cached by
/// SQLite, not once per row.
pub(crate) fn register(conn: &Connection) -> rusqlite::Result<()> {
    conn.create_scalar_function(
        SQL_FUNCTION,
        3,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        sql_match,
    )
}

fn sql_match(ctx: &Context<'_>) -> rusqlite::Result<i64> {
    let query = ctx.get_or_create_aux(0, |raw| -> rusqlite::Result<Option<SearchQuery>> {
        Ok(SearchQuery::parse(raw.as_str().unwrap_or_default()))
    })?;
    let Some(query) = query.as_ref() else {
        return Ok(0);
    };
    // A NULL or non-text column is no text, not an error: one odd row must
    // not abort the whole search.
    let text = |i: usize| ctx.get_raw(i).as_str().unwrap_or_default();
    Ok(query
        .classify(text(1), text(2))
        .map_or(0, |kind| kind as i64))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whether an article matches at all.
    fn hit(q: &SearchQuery, title: &str, body: &str) -> bool {
        q.classify(title, body).is_some()
    }

    #[test]
    fn an_empty_query_is_no_query() {
        assert_eq!(SearchQuery::parse(""), None);
        assert_eq!(SearchQuery::parse(" \t\n "), None);
    }

    #[test]
    fn terms_are_split_on_whitespace_and_folded() {
        let q = SearchQuery::parse("  Rust   SAILFISH ").unwrap();
        assert_eq!(q.terms(), ["rust", "sailfish"]);
    }

    #[test]
    fn markup_is_not_text() {
        let q = SearchQuery::parse("href").unwrap();
        assert!(
            !hit(&q, "", r#"<p><a href="https://x.example/">a link</a></p>"#),
            "an attribute name is not something the reader saw"
        );
        let q = SearchQuery::parse("div").unwrap();
        assert!(!hit(&q, "", "<div>text</div>"));
    }

    #[test]
    fn every_term_must_match_somewhere() {
        let q = SearchQuery::parse("harbour dusk").unwrap();
        assert!(hit(&q, "A harbour", "<p>at dusk</p>"));
        assert!(!hit(&q, "A harbour", "<p>at dawn</p>"));
        // Both terms left for the body, and only one of them in it.
        assert!(!hit(&q, "", "<p>the harbour at dawn</p>"));
        assert!(hit(&q, "", "<p>the harbour at dusk</p>"));
    }

    #[test]
    fn a_title_holding_any_term_is_where_an_article_matched() {
        let q = SearchQuery::parse("harbour dusk").unwrap();
        let kind = |title, body| q.classify(title, body);
        assert_eq!(kind("Dusk", "harbour"), Some(MatchKind::Title));
        assert_eq!(kind("Harbour at dusk", ""), Some(MatchKind::Title));
        assert_eq!(kind("Evening", "harbour at dusk"), Some(MatchKind::Text));
        assert_eq!(kind("Evening", "harbour"), None);
        assert_eq!(kind("Dusk", "dawn"), None);
    }

    #[test]
    fn a_feed_matches_when_its_name_holds_every_term() {
        let q = SearchQuery::parse("harbour GAZ").unwrap();
        assert!(q.matches("The Harbour Gazette"));
        assert!(!q.matches("The Harbour Times"), "every term, not any");
        assert!(!q.matches(""));
        let q = SearchQuery::parse("ääni").unwrap();
        assert!(q.matches("ÄÄNI ja kuva"), "case is ignored beyond ASCII");
    }

    #[test]
    fn the_author_is_not_searched() {
        // Not a parameter any more: what a row cannot show, a search does
        // not find an article by. This pins the SQL function's arity too.
        let conn = Connection::open_in_memory().unwrap();
        register(&conn).unwrap();
        assert!(conn
            .query_row(
                "SELECT vuo_search_kind('jane', '', 'Jane Doe', '')",
                [],
                |r| r.get::<_, i64>(0),
            )
            .is_err());
    }

    #[test]
    fn case_is_ignored_beyond_ascii() {
        let q = SearchQuery::parse("ÄÄNI").unwrap();
        assert!(hit(&q, "Hyvä ääni", ""));
        assert!(hit(&q, "", "<p>Hyvä ääni</p>"));
    }

    #[test]
    fn character_references_are_decoded() {
        let q = SearchQuery::parse("at&t").unwrap();
        assert!(hit(&q, "", "<p>AT&amp;T</p>"));
        let q = SearchQuery::parse("café").unwrap();
        assert!(hit(&q, "", "<p>Caf&#233;s and caf&#xE9;</p>"));
    }

    #[test]
    fn inline_tags_do_not_split_words_and_block_tags_do() {
        assert_eq!(html_text("<b>Wo</b>rd"), "Word");
        assert_eq!(html_text("<p>one</p><p>two</p>").trim(), "one  two");
        let q = SearchQuery::parse("onetwo").unwrap();
        assert!(!hit(&q, "", "<p>one</p><p>two</p>"));
    }

    #[test]
    fn broken_markup_and_stray_ampersands_are_survivable() {
        assert_eq!(html_text("a & b"), "a & b");
        assert_eq!(html_text("a &unknown; b"), "a &unknown; b");
        assert_eq!(html_text("a &#xZZ; b"), "a &#xZZ; b");
        assert_eq!(html_text("a &#1114112; b"), "a &#1114112; b");
        assert_eq!(html_text("trailing &"), "trailing &");
        assert_eq!(html_text("text <unterminated tag"), "text ");
        assert_eq!(html_text("&nbsp;x"), " x");
    }

    #[test]
    fn a_highlight_bolds_every_term_in_any_case() {
        let q = SearchQuery::parse("harbour dusk").unwrap();
        assert_eq!(
            q.highlight("Harbour at DUSK, the harbour"),
            "<b>Harbour</b> at <b>DUSK</b>, the <b>harbour</b>"
        );
        assert_eq!(q.highlight("Nothing here"), "Nothing here");
        assert_eq!(q.highlight(""), "");
    }

    #[test]
    fn a_highlight_is_escaped_so_only_its_own_bold_is_markup() {
        // §9.3: the title is foreign text, and the result is StyledText.
        let q = SearchQuery::parse("b").unwrap();
        assert_eq!(
            q.highlight("<b>A&B</b>"),
            "&lt;<b>b</b>&gt;A&amp;<b>B</b>&lt;/<b>b</b>&gt;"
        );
        let q = SearchQuery::parse("&").unwrap();
        assert_eq!(q.highlight("AT&T"), "AT<b>&amp;</b>T");
    }

    #[test]
    fn overlapping_terms_make_one_bold_run() {
        let q = SearchQuery::parse("harb bour").unwrap();
        assert_eq!(q.highlight("Harbour"), "<b>Harbour</b>");
        // Touching, too: two runs side by side would be one in bold anyway.
        let q = SearchQuery::parse("har bour").unwrap();
        assert_eq!(q.highlight("harbour"), "<b>harbour</b>");
        let q = SearchQuery::parse("aa").unwrap();
        assert_eq!(q.highlight("aaa"), "<b>aaa</b>");
    }

    #[test]
    fn a_highlight_marks_the_original_characters_beyond_ascii() {
        let q = SearchQuery::parse("ääni").unwrap();
        assert_eq!(q.highlight("HYVÄ ÄÄNI!"), "HYVÄ <b>ÄÄNI</b>!");
        // A capital that folds to two characters: a match on the first of
        // them stops short of it rather than cutting it in half.
        let q = SearchQuery::parse("i").unwrap();
        assert_eq!(q.highlight("\u{130}x i"), "\u{130}x <b>i</b>");
        // The Greek final sigma folds in context, and still lines up.
        let q = SearchQuery::parse("οδος").unwrap();
        assert_eq!(q.highlight("ΟΔΟΣ ΟΔΟΣ"), "<b>ΟΔΟΣ</b> <b>ΟΔΟΣ</b>");
    }

    #[test]
    fn an_excerpt_is_the_text_around_the_first_match() {
        let q = SearchQuery::parse("harbour").unwrap();
        assert_eq!(
            q.excerpt("<p>A walk by the\n\n   <b>harbour</b>.</p>")
                .as_deref(),
            Some("A walk by the harbour."),
            "short enough to be whole: no ellipsis, and whitespace collapsed"
        );
        assert_eq!(q.excerpt("<p>No match in here</p>"), None);
        assert_eq!(
            q.excerpt(r#"<a href="harbour">x</a>"#),
            None,
            "the markup is not text"
        );

        let before = "word ".repeat(40);
        let after = " more".repeat(80);
        let body = format!("<p>{before}harbour{after}</p>");
        let cut = q.excerpt(&body).unwrap();
        assert!(
            cut.starts_with('\u{2026}'),
            "the body goes on before: {cut}"
        );
        assert!(cut.ends_with('\u{2026}'), "and after: {cut}");
        let at = cut.find("harbour").expect("the match is in it");
        let lead = cut.get(..at).unwrap().chars().count();
        assert!(
            (2..=EXCERPT_LEAD + 1).contains(&lead),
            "a few words lead up to the match, not a whole line: {lead} in {cut:?}"
        );
        assert!(
            cut.chars().count() <= EXCERPT_LENGTH + 2,
            "a few lines, not the body: {cut:?}"
        );
        // Cut at word boundaries, so no word is cut in half at either end.
        let inner = cut.trim_matches('\u{2026}');
        assert!(inner.starts_with("word "), "{cut:?}");
        assert!(
            inner.ends_with(" more") || inner.ends_with("harbour"),
            "{cut:?}"
        );
    }

    #[test]
    fn an_excerpt_counts_characters_not_bytes() {
        let q = SearchQuery::parse("цель").unwrap();
        let body = format!("<p>{}цель{}</p>", "слово ".repeat(30), " ещё".repeat(80));
        let cut = q.excerpt(&body).unwrap();
        assert!(cut.chars().count() > EXCERPT_LENGTH / 2, "{cut:?}");
        assert!(cut.chars().count() <= EXCERPT_LENGTH + 2, "{cut:?}");
    }

    #[test]
    fn an_excerpt_of_a_match_at_the_start_has_no_leading_ellipsis() {
        let q = SearchQuery::parse("harbour").unwrap();
        let body = format!("<p>Harbour{}</p>", " more".repeat(80));
        let cut = q.excerpt(&body).unwrap();
        assert!(cut.starts_with("Harbour more"), "{cut:?}");
        // One word too long to cut at: shown from the match.
        let body = format!("<p>{}harbour</p>", "x".repeat(100));
        assert_eq!(q.excerpt(&body).as_deref(), Some("\u{2026}harbour"));
    }

    #[test]
    fn the_sql_function_classifies_like_the_rust_one() {
        let conn = Connection::open_in_memory().unwrap();
        register(&conn).unwrap();
        let kind = |sql: &str, args: [&str; 3]| -> i64 {
            conn.query_row(sql, args, |r| r.get(0)).unwrap()
        };
        let sql = "SELECT vuo_search_kind(?1, ?2, ?3)";
        assert_eq!(kind(sql, ["dusk", "Title", "<p>At DUSK</p>"]), 3);
        assert_eq!(kind(sql, ["dusk", "Dusk", ""]), 1);
        assert_eq!(kind(sql, ["dusk", "", "dawn"]), 0);
        assert_eq!(
            kind(sql, ["  ", "anything", ""]),
            0,
            "an empty query matches nothing, not everything"
        );
        let null: i64 = conn
            .query_row("SELECT vuo_search_kind('x', NULL, NULL)", [], |r| r.get(0))
            .unwrap();
        assert_eq!(null, 0, "a NULL column is no text, not an error");
    }
}
