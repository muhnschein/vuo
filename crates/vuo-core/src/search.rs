//! Searching the local mirror.
//!
//! §5: the mirror is the UI's single source of truth and the UI never waits on
//! the network, so search runs against what has been synced rather than asking
//! the server. It works offline, and it finds exactly what the lists can show.
//!
//! # What matches
//!
//! A query is split on whitespace into terms, and an article matches when
//! EVERY term appears somewhere in its title, its author, its feed's name or
//! the TEXT of its body. Case is ignored. A term is a plain substring: no
//! wildcards, no operators, so `50%` and `C++` mean what they say.
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
//! The matching runs inside SQLite as `vuo_search_match(...)`, registered on
//! every connection by [`crate::db::Database`]. The alternative -- reading
//! every body out to filter it in Rust -- is the shape `store::EntryListRow`
//! exists to avoid: it carried every body in the mirror through memory at
//! once. Inside the query each body is looked at once, in SQLite's own buffer,
//! and dropped before the next row.

use rusqlite::functions::{Context, FunctionFlags};
use rusqlite::Connection;

/// The SQL name the matcher is registered under. `store::search_entries`
/// spells it out in its statement, which is a literal by rule (§9.4).
const SQL_FUNCTION: &str = "vuo_search_match";

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

    /// Whether an article matches: every term somewhere in its title, its
    /// author, its feed's name or the text of its body.
    ///
    /// The body is the expensive part, so it is stripped and folded only when
    /// the short fields have not already accounted for every term.
    #[must_use]
    pub fn matches(&self, title: &str, author: &str, feed: &str, content_html: &str) -> bool {
        let short = fold(&[title, author, feed].join("\n"));
        let rest: Vec<&String> = self
            .terms
            .iter()
            .filter(|t| !short.contains(t.as_str()))
            .collect();
        if rest.is_empty() {
            return true;
        }
        let mut body = html_text(content_html);
        if rest.iter().all(|t| t.is_ascii()) {
            // In place, and several times faster than `to_lowercase`. The
            // same answer for an ASCII term, but for a handful of exotic code
            // points that `to_lowercase` folds INTO ASCII (the Kelvin sign
            // becomes `k`), which no reader will miss.
            body.make_ascii_lowercase();
        } else {
            body = fold(&body);
        }
        rest.iter().all(|t| body.contains(t.as_str()))
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
/// `vuo_search_match(query, title, author, feed_title, content)` is 1 when the
/// article matches, 0 when it does not, and 0 for a query with no terms. The
/// query is parsed once per statement and cached by SQLite, not once per row.
pub(crate) fn register(conn: &Connection) -> rusqlite::Result<()> {
    conn.create_scalar_function(
        SQL_FUNCTION,
        5,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        sql_match,
    )
}

fn sql_match(ctx: &Context<'_>) -> rusqlite::Result<bool> {
    let query = ctx.get_or_create_aux(0, |raw| -> rusqlite::Result<Option<SearchQuery>> {
        Ok(SearchQuery::parse(raw.as_str().unwrap_or_default()))
    })?;
    let Some(query) = query.as_ref() else {
        return Ok(false);
    };
    // A NULL or non-text column is no text, not an error: one odd row must
    // not abort the whole search.
    let text = |i: usize| ctx.get_raw(i).as_str().unwrap_or_default();
    Ok(query.matches(text(1), text(2), text(3), text(4)))
}

#[cfg(test)]
mod tests {
    use super::*;

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
            !q.matches(
                "",
                "",
                "",
                r#"<p><a href="https://x.example/">a link</a></p>"#
            ),
            "an attribute name is not something the reader saw"
        );
        let q = SearchQuery::parse("div").unwrap();
        assert!(!q.matches("", "", "", "<div>text</div>"));
    }

    #[test]
    fn every_term_must_match_somewhere() {
        let q = SearchQuery::parse("harbour dusk").unwrap();
        assert!(q.matches("A harbour", "", "", "<p>at dusk</p>"));
        assert!(!q.matches("A harbour", "", "", "<p>at dawn</p>"));
        // Both terms left for the body, and only one of them in it.
        assert!(!q.matches("", "", "", "<p>the harbour at dawn</p>"));
        assert!(q.matches("", "", "", "<p>the harbour at dusk</p>"));
    }

    #[test]
    fn author_and_feed_are_searched() {
        let q = SearchQuery::parse("tagesschau").unwrap();
        assert!(q.matches("", "", "Tagesschau", ""));
        let q = SearchQuery::parse("jane").unwrap();
        assert!(q.matches("", "Jane Doe", "", ""));
    }

    #[test]
    fn case_is_ignored_beyond_ascii() {
        let q = SearchQuery::parse("ÄÄNI").unwrap();
        assert!(q.matches("Hyvä ääni", "", "", ""));
    }

    #[test]
    fn character_references_are_decoded() {
        let q = SearchQuery::parse("at&t").unwrap();
        assert!(q.matches("", "", "", "<p>AT&amp;T</p>"));
        let q = SearchQuery::parse("café").unwrap();
        assert!(q.matches("", "", "", "<p>Caf&#233;s and caf&#xE9;</p>"));
    }

    #[test]
    fn inline_tags_do_not_split_words_and_block_tags_do() {
        assert_eq!(html_text("<b>Wo</b>rd"), "Word");
        assert_eq!(html_text("<p>one</p><p>two</p>").trim(), "one  two");
        let q = SearchQuery::parse("onetwo").unwrap();
        assert!(!q.matches("", "", "", "<p>one</p><p>two</p>"));
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
    fn the_sql_function_matches_like_the_rust_one() {
        let conn = Connection::open_in_memory().unwrap();
        register(&conn).unwrap();
        let hit: bool = conn
            .query_row(
                "SELECT vuo_search_match(?1, ?2, '', '', ?3)",
                ["dusk", "Title", "<p>At DUSK</p>"],
                |r| r.get(0),
            )
            .unwrap();
        assert!(hit);
        let empty: bool = conn
            .query_row(
                "SELECT vuo_search_match('  ', 'anything', '', '', '')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!empty, "an empty query matches nothing, not everything");
        let null: bool = conn
            .query_row(
                "SELECT vuo_search_match('x', NULL, NULL, NULL, NULL)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!null, "a NULL column is no text, not an error");
    }
}
