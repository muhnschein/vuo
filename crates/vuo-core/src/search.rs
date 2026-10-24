//! Searching local mirror.
//!
//! §5: mirror = UI's single source of truth, UI never waits on network: search
//! runs against synced data, not server. Works offline, finds exactly what
//! lists can show.
//!
//! # What matches
//!
//! Query split on whitespace into terms; article matches when EVERY term is in
//! title or body TEXT. Case ignored. Term = plain substring: no wildcards, no
//! operators; `50%` and `C++` mean what they say.
//!
//! Author not searched: row doesn't show it; hit by author has nothing on
//! screen saying why.
//!
//! Feed name neither. Feed's name finds FEED (every term in name,
//! [`SearchQuery::matches`]): typing feed name = after the feed, not its latest
//! articles.
//!
//! # Where it matched
//!
//! Results grouped by WHERE matched ([`MatchKind`]): title hits first (most
//! likely wanted). Article kind = title when title holds any term, else body
//! text; feeds own group. Result rows: [`SearchQuery::highlight`] marks terms
//! in title or feed name; [`SearchQuery::excerpt`] cuts lines around body
//! match.
//!
//! "Body text" is the point. Bodies stored as Miniflux HTML; substring over raw
//! markup matches every article for `div`, `href`, `class`, and misses escaped
//! text: Miniflux escapes `&`, `<`, `>`, so `AT&T` stored as `AT&amp;T`.
//! [`html_text`] strips markup, decodes references that matter, before
//! comparing.
//!
//! # Why SQL function
//!
//! Matching runs inside SQLite as `vuo_search_kind(...)`, registered per
//! connection by [`crate::db::Database`]. Alternative (read every body into
//! Rust to filter) = shape `store::EntryListRow` exists to avoid: every body in
//! memory at once. In-query: each body seen once in SQLite's buffer, dropped
//! before next row.

use rusqlite::functions::{Context, FunctionFlags};
use rusqlite::Connection;

/// SQL name matcher registered under. `store::search_entries` spells it out:
/// statement literal by rule (§9.4).
const SQL_FUNCTION: &str = "vuo_search_kind";

/// Where result matched = group it's listed in. Group order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MatchKind {
    /// An article whose title holds a term.
    Title = 1,
    /// FEED whose name holds every term (see [`SearchQuery::matches`]). Never
    /// article: [`SearchQuery::classify`] doesn't return it.
    Feed = 2,
    /// An article whose body's text holds a term, and whose title does not.
    Text = 3,
}

impl MatchKind {
    /// Kind as SQL returns it: 1 or 3 for article, as
    /// [`SearchQuery::classify`]; else no match.
    #[must_use]
    pub fn from_sql(value: i64) -> Option<MatchKind> {
        match value {
            1 => Some(MatchKind::Title),
            3 => Some(MatchKind::Text),
            _ => None,
        }
    }
}

/// Body chars excerpt shows before match: ~half phone line, so match lands on
/// excerpt's first line with lead-in.
const EXCERPT_LEAD: usize = 40;
/// Max excerpt length, chars. Little over list's three lines: view decides cut,
/// not this.
const EXCERPT_LENGTH: usize = 200;

/// A parsed query: its terms, case-folded. Never empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    terms: Vec<String>,
}

impl SearchQuery {
    /// Parse what reader typed. `None` = nothing to search; callers read as "no
    /// results", not "everything".
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

    /// Where article matches ([`MatchKind::Title`] or [`MatchKind::Text`]), or
    /// `None`: every term must be in title or body text.
    ///
    /// Body expensive: stripped and folded only when title hasn't covered every
    /// term.
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
                // In place, several times faster than `to_lowercase`. Same for
                // ASCII term, except few exotic code points `to_lowercase`
                // folds INTO ASCII (Kelvin sign → `k`); no reader misses them.
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

    /// `text` as Qt `StyledText`, every term occurrence bold.
    ///
    /// Title or feed name in result row. FOREIGN text, escaped: only markup =
    /// added `<b>`, letting QML render `StyledText` (§9.3).
    #[must_use]
    pub fn highlight(&self, text: &str) -> String {
        let mut out = String::with_capacity(text.len().saturating_add(16));
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

    /// Text around first body match, plain; `None` when no term in text.
    ///
    /// Few words before, few lines after, cut at word boundaries, ellipsis
    /// where body continues. Whitespace collapsed: body line breaks are
    /// markup's, not reader's.
    #[must_use]
    pub fn excerpt(&self, content_html: &str) -> Option<String> {
        let text = html_text(content_html)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let (first, _) = self.spans(&text).into_iter().next()?;

        // Chars from here: byte count would cut Cyrillic line to half English
        // length.
        let chars: Vec<(usize, char)> = text.char_indices().collect();
        let at = chars.iter().position(|&(i, _)| i == first)?;
        let mut from = at.saturating_sub(EXCERPT_LEAD);
        if from > 0 {
            // Forward to word start, never past match.
            from = (from..at)
                .find(|&i| chars.get(i.wrapping_sub(1)).is_some_and(|&(_, c)| c == ' '))
                .unwrap_or(at);
        }
        let mut to = from.saturating_add(EXCERPT_LENGTH).min(chars.len());
        if to < chars.len() {
            // Back to word end, unless that loses match.
            if let Some(space) = (at.saturating_add(1)..to)
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

    /// Where terms are in `text`: byte ranges of ORIGINAL text, sorted,
    /// overlapping and touching merged.
    fn spans(&self, text: &str) -> Vec<(usize, usize)> {
        // Folded text searched; every folded byte maps back to source char.
        // `to_lowercase` folds a char to same byte count alone or in string
        // (only context rule, Greek final sigma, swaps two-byte sigma for
        // another): per-char map fits whole-string fold. If ever not, nothing
        // marked: missing bold beats misplaced.
        let folded = fold(text);
        let mut origin = Vec::with_capacity(folded.len().saturating_add(1));
        for (at, ch) in text.char_indices() {
            let width: usize = ch.to_lowercase().map(char::len_utf8).sum();
            origin.extend(std::iter::repeat_n(at, width));
        }
        origin.push(text.len());
        if origin.len() != folded.len().saturating_add(1) {
            return Vec::new();
        }

        let mut found: Vec<(usize, usize)> = Vec::new();
        for term in &self.terms {
            let mut from = 0;
            while let Some(hit) = folded.get(from..).and_then(|rest| rest.find(term.as_str())) {
                let start = from.saturating_add(hit);
                let end = start.saturating_add(term.len());
                // END maps to start of char after match: match ending inside
                // one char's folding (dotted capital I → two) stops short of
                // it.
                if let (Some(&a), Some(&b)) = (origin.get(start), origin.get(end)) {
                    if a < b {
                        found.push((a, b));
                    }
                }
                // Next char; past end, `get` above ends loop.
                from = start.saturating_add(1);
                while from < folded.len() && !folded.is_char_boundary(from) {
                    from = from.saturating_add(1);
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
/// `to_lowercase`, not ASCII folding (SQLite `LIKE`): German or Finnish reader
/// typing `ä` must find `Ä`.
fn fold(s: &str) -> String {
    s.to_lowercase()
}

/// Inline elements: closing one doesn't end word, removed without space.
/// `<b>Wo</b>rd` = `Word`.
const INLINE: &[&str] = &[
    "a", "abbr", "b", "bdi", "bdo", "cite", "code", "data", "dfn", "em", "i", "kbd", "mark", "q",
    "s", "samp", "small", "span", "strike", "strong", "sub", "sup", "time", "u", "var", "wbr",
];

/// HTML fragment text, for searching.
///
/// Not parser, not content transform: nothing rendered; only must get right
/// which chars reader would have seen. Tags removed (block-level leave space:
/// two paragraphs don't merge into one word); references sanitised bodies
/// contain decoded. Unrecognised passes through as text: worst case, term
/// findable that shouldn't be.
#[must_use]
pub fn html_text(html: &str) -> String {
    // Scanned as BYTES: cheap enough for every body each search. `<`, `>`, `&`,
    // `;` are ASCII; ASCII byte in UTF-8 always whole char, so every index =
    // char boundary, every slice below = infallible `get`.
    let bytes = html.as_bytes();
    let mut out = String::with_capacity(html.len());
    let mut copied = 0;
    let mut at = 0;
    while let Some(found) = bytes
        .get(at..)
        .and_then(|b| b.iter().position(|&c| c == b'<' || c == b'&'))
    {
        let mark = at.saturating_add(found);
        out.push_str(html.get(copied..mark).unwrap_or_default());
        if bytes.get(mark) == Some(&b'<') {
            // Unterminated tag ends text: rest not something reader saw.
            let after = mark.saturating_add(1);
            let Some(len) = bytes
                .get(after..)
                .and_then(|b| b.iter().position(|&c| c == b'>'))
            else {
                return out;
            };
            let close = after.saturating_add(len);
            if !is_inline(html.get(after..close).unwrap_or_default()) {
                out.push(' ');
            }
            at = close.saturating_add(1);
        } else {
            let after = mark.saturating_add(1);
            if let Some((ch, used)) = decode_entity(html.get(after..).unwrap_or_default()) {
                out.push(ch);
                at = after.saturating_add(used);
            } else {
                out.push('&');
                at = after;
            }
        }
        copied = at;
    }
    out.push_str(html.get(copied..).unwrap_or_default());
    out
}

/// Whether tag text (between `<` and `>`) names inline element.
///
/// Compared in place, no lowercased copy: runs per tag in every body.
fn is_inline(tag: &str) -> bool {
    let tag = tag.strip_prefix('/').unwrap_or(tag);
    let end = tag
        .find(|c: char| !c.is_ascii_alphanumeric())
        .unwrap_or(tag.len());
    let name = tag.get(..end).unwrap_or_default();
    INLINE.iter().any(|n| n.eq_ignore_ascii_case(name))
}

/// Decode character reference at start of `s` (text after `&`).
///
/// Returns char and bytes used, incl. `;`. Only references ending in `;` within
/// short distance decoded; else left as text.
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
    Some((ch, semi.checked_add(1)?))
}

/// Register [`SQL_FUNCTION`] on connection.
///
/// `vuo_search_kind(query, title, content)` = [`MatchKind`] as number when
/// article matches, 0 when not, 0 for query without terms. Query parsed once
/// per statement, cached by SQLite, not per row.
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
    // NULL or non-text column = no text, not error: one odd row mustn't abort
    // search.
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
        // Both terms left for body, only one in it.
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
        // Not a parameter: what row can't show, search doesn't find by. Pins
        // SQL function arity too.
        let conn = Connection::open_in_memory().unwrap();
        register(&conn).unwrap();
        conn.query_row(
            "SELECT vuo_search_kind('jane', '', 'Jane Doe', '')",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_err();
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
        // §9.3: title foreign, result StyledText.
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
        // Touching too: two adjacent runs bold anyway.
        let q = SearchQuery::parse("har bour").unwrap();
        assert_eq!(q.highlight("harbour"), "<b>harbour</b>");
        let q = SearchQuery::parse("aa").unwrap();
        assert_eq!(q.highlight("aaa"), "<b>aaa</b>");
    }

    #[test]
    fn a_highlight_marks_the_original_characters_beyond_ascii() {
        let q = SearchQuery::parse("ääni").unwrap();
        assert_eq!(q.highlight("HYVÄ ÄÄNI!"), "HYVÄ <b>ÄÄNI</b>!");
        // Capital folding to two chars: match on first stops short, doesn't cut
        // it.
        let q = SearchQuery::parse("i").unwrap();
        assert_eq!(q.highlight("\u{130}x i"), "\u{130}x <b>i</b>");
        // Greek final sigma folds in context, still lines up.
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
        // Cut at word boundaries: no half word at either end.
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
        // One word too long to cut: shown from match.
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
