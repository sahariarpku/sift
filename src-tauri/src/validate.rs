//! Quote validation (principle 3, §8.8). A quote counts only if it appears
//! verbatim in the source after normalising whitespace, line-break
//! hyphenation and typographic punctuation. Case is kept: "verbatim" means
//! the same words in the same order.

/// Normalises text for matching.
pub fn normalise(s: &str) -> String {
    let mut t = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{00AD}' => {} // soft hyphen
            '\u{2018}' | '\u{2019}' | '\u{201B}' | '\u{2032}' => t.push('\''),
            '\u{201C}' | '\u{201D}' | '\u{201F}' | '\u{2033}' => t.push('"'),
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2212}' => {
                t.push('-')
            }
            '\u{FB01}' => t.push_str("fi"),
            '\u{FB02}' => t.push_str("fl"),
            '\u{2026}' => t.push_str("..."),
            '-' => {
                // "experi-\nence" -> "experience": a hyphen followed by a line break joins the word.
                let mut look = chars.clone();
                let mut saw_newline = false;
                while let Some(&n) = look.peek() {
                    if n == '\n' || n == '\r' {
                        saw_newline = true;
                        look.next();
                    } else if n == ' ' || n == '\t' {
                        look.next();
                    } else {
                        break;
                    }
                }
                if saw_newline && look.peek().is_some_and(|n| n.is_lowercase()) {
                    chars = look;
                } else {
                    t.push('-');
                }
            }
            c if c.is_whitespace() => t.push(' '),
            c => t.push(c),
        }
    }
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_edges(q: &str) -> &str {
    q.trim()
        .trim_matches(|c: char| {
            c == '"' || c == '\'' || c == '\u{201C}' || c == '\u{201D}' || c == '…'
        })
        .trim()
}

/// True if `quote` appears verbatim in `text`.
pub fn quote_in(quote: &str, text: &str) -> bool {
    let q = normalise(strip_edges(quote));
    if q.len() < 3 {
        return false;
    }
    let t = normalise(text);
    if t.contains(&q) {
        return true;
    }
    // Models often mark an omission with "..."; accept when every part appears in order.
    if q.contains("...") {
        let mut from = 0;
        for part in q.split("...").map(str::trim).filter(|p| !p.is_empty()) {
            match t[from..].find(part) {
                Some(i) => from += i + part.len(),
                None => return false,
            }
        }
        return true;
    }
    false
}

/// Finds the page (1-based) holding `quote`: the stated page first, then its
/// neighbours joined (a quote across a page break), then the adjacent pages.
/// Returns the page where the quote starts.
pub fn locate(quote: &str, pages: &[String], stated_page: Option<i64>) -> Option<i64> {
    let n = pages.len() as i64;
    if n == 0 {
        return None;
    }
    let get = |p: i64| {
        pages
            .get((p - 1) as usize)
            .map(String::as_str)
            .unwrap_or("")
    };
    if let Some(p) = stated_page.filter(|p| (1..=n).contains(p)) {
        // Whole quote on one page first, so a quote on the next page is not
        // credited to the stated page through a page-break join.
        for cand in [p, p + 1, p - 1] {
            if (1..=n).contains(&cand) && quote_in(quote, get(cand)) {
                return Some(cand);
            }
        }
        if p < n && quote_in(quote, &format!("{} {}", get(p), get(p + 1))) {
            return Some(p);
        }
        if p > 1 && quote_in(quote, &format!("{} {}", get(p - 1), get(p))) {
            return Some(p - 1);
        }
        return None;
    }
    // No page given: search every page.
    (1..=n)
        .find(|&p| quote_in(quote, get(p)))
        .or_else(|| (1..n).find(|&p| quote_in(quote, &format!("{} {}", get(p), get(p + 1)))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitespace_and_typography() {
        assert!(quote_in(
            "We recruited 24  first-year\nundergraduates",
            "We recruited 24 first-year undergraduates who were the first"
        ));
        assert!(quote_in(
            "\u{201C}I didn\u{2019}t belong\u{201D}",
            "she said \"I didn't belong\" there"
        ));
        assert!(quote_in("an 8\u{2013}10 week", "an 8-10 week course"));
        assert!(!quote_in(
            "We recruited 25 first-year",
            "We recruited 24 first-year undergraduates"
        ));
        assert!(
            !quote_in("we recruited 24", "We recruited 24"),
            "case matters"
        );
        assert!(!quote_in("a", "a b c"), "too short to count");
    }

    #[test]
    fn hyphenation_across_line_breaks() {
        let text = "students reported a strong sense of be-\nlonging after the first term";
        assert!(quote_in("a strong sense of belonging after", text));
        // A real hyphen before a capital or number is kept.
        assert!(quote_in("mixed-methods", "a mixed-methods design"));
        assert!(!quote_in("mixedmethods", "a mixed-\nMethods design"));
    }

    #[test]
    fn ellipsis_parts_in_order() {
        let t =
            "Participants felt isolated in seminars. Several described group work as exhausting.";
        assert!(quote_in("felt isolated ... group work as exhausting", t));
        assert!(!quote_in("group work ... felt isolated", t));
    }

    #[test]
    fn locates_pages_and_page_breaks() {
        let pages = vec![
            "Introduction text.".to_owned(),
            "Methods: we interviewed 18 Chinese postgraduate".to_owned(),
            "students at two universities in England.".to_owned(),
            "Findings: loneliness was common.".to_owned(),
        ];
        assert_eq!(locate("loneliness was common", &pages, Some(4)), Some(4));
        assert_eq!(
            locate("loneliness was common", &pages, Some(3)),
            Some(4),
            "adjacent page allowed"
        );
        assert_eq!(
            locate(
                "18 Chinese postgraduate students at two universities",
                &pages,
                Some(2)
            ),
            Some(2),
            "across a page break"
        );
        assert_eq!(
            locate(
                "18 Chinese postgraduate students at two universities",
                &pages,
                Some(3)
            ),
            Some(2)
        );
        assert_eq!(
            locate("loneliness was common", &pages, Some(1)),
            None,
            "two pages away is not accepted"
        );
        assert_eq!(locate("loneliness was common", &pages, None), Some(4));
        assert_eq!(locate("not there at all", &pages, None), None);
    }
}
