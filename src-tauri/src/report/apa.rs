//! APA 7 references and in-text citations for the manuscript.
//!
//! Rules followed (APA Publication Manual, 7th ed.):
//! - Reference list authors: "Surname, I. I.", up to 20 authors joined with
//!   commas and "&"; with 21 or more, the first 19, an ellipsis, then the last
//!   (section 9.8).
//! - In-text: one author "Smith, 2021"; two "Smith & Jones, 2021"; three or
//!   more "Smith et al., 2021" (section 8.17).
//! - Same author(s) and year: letters after the year, in reference-list
//!   order (section 8.19).
//! - Studies included in the review are marked with an asterisk in the
//!   reference list (section 9.52).
//!
//! Bibliographic records carry no volume, issue or pages, so references to
//! included studies give authors, year, title, source and DOI only.

use serde::Serialize;

use crate::records::RecordRow;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Reference {
    /// Parenthetical form without the brackets: "Smith et al., 2021a".
    pub cite: String,
    /// Narrative form: "Smith et al. (2021a)".
    pub narrative: String,
    /// The reference-list author element, e.g. "Smith, J. A., & Jones, B.".
    pub authors: String,
    /// "2021a" or "n.d.".
    pub year: String,
    pub title: String,
    /// Journal or other source, set in italics.
    pub source: Option<String>,
    /// Volume, issue and pages or article number, when known, e.g. "*8*, Article 45".
    /// Italic parts are wrapped in asterisks.
    pub detail: Option<String>,
    pub doi: Option<String>,
    /// A study included in the review (asterisk in the reference list).
    pub included: bool,
    pub record_id: Option<String>,
}

impl Reference {
    /// The reference as plain text, without italics.
    pub fn plain(&self) -> String {
        let mut s = format!(
            "{}{} ({}). {}",
            if self.included { "*" } else { "" },
            self.authors,
            self.year,
            end_with_period(&self.title)
        );
        if let Some(src) = &self.source {
            s.push(' ');
            s.push_str(src);
            if let Some(d) = &self.detail {
                s.push_str(", ");
                s.push_str(&d.replace('*', ""));
            }
            s.push('.');
        }
        if let Some(d) = &self.doi {
            s.push_str(&format!(" https://doi.org/{d}"));
        }
        s
    }
}

fn end_with_period(s: &str) -> String {
    let t = s.trim();
    if t.ends_with(['.', '?', '!']) {
        t.to_owned()
    } else {
        format!("{t}.")
    }
}

const MINOR: [&str; 22] = [
    "a", "an", "the", "and", "but", "or", "nor", "for", "so", "yet", "as", "at", "by", "in", "of",
    "off", "on", "per", "to", "up", "via", "vs.",
];

/// APA title case (section 6.17): major words and words of four or more
/// letters are capitalised; short minor words stay lower case except first
/// and after a colon. Words already holding capitals (acronyms, names) keep them.
pub fn title_case(s: &str) -> String {
    let mut out = Vec::new();
    let mut start = true;
    for word in s.split_whitespace() {
        let lower = word.to_lowercase();
        let has_caps = word.chars().skip(1).any(char::is_uppercase);
        let w = if has_caps || (!start && MINOR.contains(&lower.as_str())) {
            word.to_owned()
        } else {
            word.split('-')
                .map(|part| {
                    let mut c = part.chars();
                    match c.next() {
                        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                        None => String::new(),
                    }
                })
                .collect::<Vec<_>>()
                .join("-")
        };
        start = word.ends_with(':');
        out.push(w);
    }
    out.join(" ")
}

const SUFFIXES: [&str; 4] = ["Jr.", "Jr", "Sr.", "II"];

/// Splits a name as a database gives it ("Jane A. Smith" or "Smith, Jane A.")
/// into the surname and APA initials ("J. A.").
pub fn split_name(raw: &str) -> (String, String) {
    let raw = raw.trim();
    let (surname, given) = match raw.split_once(',') {
        Some((s, g)) if !g.trim().is_empty() && !SUFFIXES.contains(&g.trim()) => {
            (s.trim().to_owned(), g.trim().to_owned())
        }
        _ => {
            let parts: Vec<&str> = raw.split_whitespace().collect();
            match parts.as_slice() {
                [] => return (String::new(), String::new()),
                [one] => return ((*one).to_owned(), String::new()),
                _ => {
                    // Surname particles stay with the surname: "Ludwig van Beethoven".
                    let mut cut = parts.len() - 1;
                    while cut > 1
                        && ["van", "von", "de", "der", "den", "da", "del", "di", "la", "le"]
                            .contains(&parts[cut - 1].to_lowercase().as_str())
                    {
                        cut -= 1;
                    }
                    (parts[cut..].join(" "), parts[..cut].join(" "))
                }
            }
        }
    };
    let initials = given
        .split_whitespace()
        .filter_map(|word| {
            let pieces: Vec<String> = word
                .split('-')
                .filter_map(|p| p.chars().find(|c| c.is_alphabetic()))
                .map(|c| format!("{}.", c.to_uppercase()))
                .collect();
            (!pieces.is_empty()).then(|| pieces.join("-"))
        })
        .collect::<Vec<_>>()
        .join(" ");
    (surname, initials)
}

fn apa_name(raw: &str) -> String {
    let (s, i) = split_name(raw);
    if i.is_empty() {
        s
    } else {
        format!("{s}, {i}")
    }
}

/// The reference-list author element.
pub fn author_list(names: &[String]) -> String {
    let names: Vec<String> = names
        .iter()
        .filter(|n| !n.trim().is_empty())
        .map(|n| apa_name(n))
        .collect();
    match names.len() {
        0 => String::new(),
        1 => names[0].clone(),
        2 => format!("{}, & {}", names[0], names[1]),
        n if n <= 20 => format!("{}, & {}", names[..n - 1].join(", "), names[n - 1]),
        n => format!("{}, . . . {}", names[..19].join(", "), names[n - 1]),
    }
}

/// The in-text author element: "Smith", "Smith & Jones" or "Smith et al.".
pub fn in_text_authors(names: &[String]) -> String {
    let surnames: Vec<String> = names
        .iter()
        .filter(|n| !n.trim().is_empty())
        .map(|n| split_name(n).0)
        .collect();
    match surnames.len() {
        0 => String::new(),
        1 => surnames[0].clone(),
        2 => format!("{} & {}", surnames[0], surnames[1]),
        _ => format!("{} et al.", surnames[0]),
    }
}

struct Draft {
    reference: Reference,
    in_text: String,
    base_year: String,
    sort_key: String,
}

fn draft(r: &RecordRow) -> Draft {
    let title = r
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or("Untitled")
        .to_owned();
    let mut authors = author_list(&r.authors);
    let mut in_text = in_text_authors(&r.authors);
    let mut title_out = title.clone();
    // No author: the title moves to the author position (section 9.12).
    let no_author = authors.is_empty();
    if no_author {
        authors = end_with_period(&title).trim_end_matches('.').to_owned();
        in_text = if title.split_whitespace().count() > 4 {
            format!(
                "\u{201c}{}\u{201d}",
                title.split_whitespace().take(4).collect::<Vec<_>>().join(" ")
            )
        } else {
            format!("\u{201c}{title}\u{201d}")
        };
        title_out = String::new();
    }
    let base_year = r
        .year
        .map(|y| y.to_string())
        .unwrap_or_else(|| "n.d.".into());
    let sort_key = format!(
        "{}\u{0}{}\u{0}{}",
        authors.to_lowercase(),
        base_year,
        title.to_lowercase()
    );
    Draft {
        reference: Reference {
            cite: String::new(),
            narrative: String::new(),
            authors,
            year: base_year.clone(),
            title: title_out,
            source: r
                .venue
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_owned),
            detail: None,
            doi: r.doi.clone(),
            included: true,
            record_id: Some(r.id.clone()),
        },
        in_text,
        base_year,
        sort_key,
    }
}

/// References to the included records, in reference-list order, with
/// year letters where the same authors and year repeat.
pub fn from_records(records: &[RecordRow]) -> Vec<Reference> {
    let mut drafts: Vec<Draft> = records.iter().map(draft).collect();
    drafts.sort_by(|a, b| a.sort_key.cmp(&b.sort_key));
    let mut seen: std::collections::HashMap<(String, String), usize> =
        std::collections::HashMap::new();
    for d in &drafts {
        *seen
            .entry((d.reference.authors.clone(), d.base_year.clone()))
            .or_default() += 1;
    }
    let mut used: std::collections::HashMap<(String, String), usize> =
        std::collections::HashMap::new();
    drafts
        .into_iter()
        .map(|mut d| {
            let key = (d.reference.authors.clone(), d.base_year.clone());
            if seen.get(&key).copied().unwrap_or(0) > 1 {
                let n = used.entry(key).or_default();
                let letter = (b'a' + (*n % 26) as u8) as char;
                *n += 1;
                d.reference.year = if d.base_year == "n.d." {
                    format!("n.d.-{letter}")
                } else {
                    format!("{}{letter}", d.base_year)
                };
            }
            d.reference.cite = format!("{}, {}", d.in_text, d.reference.year);
            d.reference.narrative = format!("{} ({})", d.in_text, d.reference.year);
            d.reference
        })
        .collect()
}

fn method(
    cite: &str,
    authors: &str,
    year: &str,
    title: &str,
    source: &str,
    detail: &str,
    doi: &str,
) -> Reference {
    Reference {
        cite: format!("{cite}, {year}"),
        narrative: format!("{cite} ({year})"),
        authors: authors.into(),
        year: year.into(),
        title: title.into(),
        source: Some(source.into()),
        detail: Some(detail.into()),
        doi: Some(doi.into()),
        included: false,
        record_id: None,
    }
}

// Metadata checked against Crossref (https://api.crossref.org/works/{doi}) on 2026-09-26.
pub fn prisma() -> Reference {
    method(
        "Page et al.",
        "Page, M. J., McKenzie, J. E., Bossuyt, P. M., Boutron, I., Hoffmann, T. C., Mulrow, C. D., Shamseer, L., Tetzlaff, J. M., Akl, E. A., Brennan, S. E., Chou, R., Glanville, J., Grimshaw, J. M., Hróbjartsson, A., Lalu, M. M., Li, T., Loder, E. W., Mayo-Wilson, E., McDonald, S., . . . Moher, D.",
        "2021",
        "The PRISMA 2020 statement: An updated guideline for reporting systematic reviews",
        "BMJ",
        "*372*, Article n71",
        "10.1136/bmj.n71",
    )
}

pub fn prisma_s() -> Reference {
    method(
        "Rethlefsen et al.",
        "Rethlefsen, M. L., Kirtley, S., Waffenschmidt, S., Ayala, A. P., Moher, D., Page, M. J., Koffel, J. B., & PRISMA-S Group",
        "2021",
        "PRISMA-S: An extension to the PRISMA statement for reporting literature searches in systematic reviews",
        "Systematic Reviews",
        "*10*, Article 39",
        "10.1186/s13643-020-01542-z",
    )
}

pub fn thomas_harden() -> Reference {
    method(
        "Thomas & Harden",
        "Thomas, J., & Harden, A.",
        "2008",
        "Methods for the thematic synthesis of qualitative research in systematic reviews",
        "BMC Medical Research Methodology",
        "*8*, Article 45",
        "10.1186/1471-2288-8-45",
    )
}

pub fn cerqual() -> Reference {
    method(
        "Lewin et al.",
        "Lewin, S., Booth, A., Glenton, C., Munthe-Kaas, H., Rashidian, A., Wainwright, M., Bohren, M. A., Tunçalp, Ö., Colvin, C. J., Garside, R., Carlsen, B., Langlois, E. V., & Noyes, J.",
        "2018",
        "Applying GRADE-CERQual to qualitative evidence synthesis findings: Introduction to the series",
        "Implementation Science",
        "*13*(Suppl. 1), Article 2",
        "10.1186/s13012-017-0688-3",
    )
}

pub fn swim() -> Reference {
    method(
        "Campbell et al.",
        "Campbell, M., McKenzie, J. E., Sowden, A., Katikireddi, S. V., Brennan, S. E., Ellis, S., Hartmann-Boyce, J., Ryan, R., Shepperd, S., Thomas, J., Welch, V., & Thomson, H.",
        "2020",
        "Synthesis without meta-analysis (SWiM) in systematic reviews: Reporting guideline",
        "BMJ",
        "*368*, Article l6890",
        "10.1136/bmj.l6890",
    )
}

pub fn grade() -> Reference {
    method(
        "Guyatt et al.",
        "Guyatt, G. H., Oxman, A. D., Vist, G. E., Kunz, R., Falck-Ytter, Y., Alonso-Coello, P., & Schünemann, H. J.",
        "2008",
        "GRADE: An emerging consensus on rating quality of evidence and strength of recommendations",
        "BMJ",
        "*336*(7650), 924\u{2013}926",
        "10.1136/bmj.39489.470347.AD",
    )
}

/// The full reference list: methods sources cited in the text plus every
/// included record, sorted alphabetically by the author element.
pub fn reference_list(methods: Vec<Reference>, included: Vec<Reference>) -> Vec<Reference> {
    let mut all: Vec<Reference> = methods.into_iter().chain(included).collect();
    all.sort_by(|a, b| {
        a.authors
            .to_lowercase()
            .cmp(&b.authors.to_lowercase())
            .then(a.year.cmp(&b.year))
    });
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: &str, authors: &[&str], year: Option<i32>, title: &str) -> RecordRow {
        RecordRow {
            id: id.into(),
            search_run_id: "s".into(),
            source: "openalex".into(),
            source_id: None,
            doi: Some(format!("10.1/{id}")),
            title: Some(title.into()),
            abstract_: None,
            authors: authors.iter().map(|a| (*a).to_owned()).collect(),
            year,
            venue: Some("Journal of Tests".into()),
            pdf_url: None,
            landing_url: None,
        }
    }

    #[test]
    fn names_in_both_database_orders() {
        assert_eq!(split_name("Jane A. Smith"), ("Smith".into(), "J. A.".into()));
        assert_eq!(split_name("Smith, Jane Ann"), ("Smith".into(), "J. A.".into()));
        assert_eq!(split_name("Jean-Paul Sartre"), ("Sartre".into(), "J.-P.".into()));
        assert_eq!(
            split_name("Ludwig van Beethoven"),
            ("van Beethoven".into(), "L.".into())
        );
        assert_eq!(split_name("Plato"), ("Plato".into(), String::new()));
    }

    #[test]
    fn author_lists_follow_apa_7() {
        let names = |n: usize| (1..=n).map(|i| format!("A{i} Author{i}")).collect::<Vec<_>>();
        assert_eq!(author_list(&names(1)), "Author1, A.");
        assert_eq!(author_list(&names(2)), "Author1, A., & Author2, A.");
        assert_eq!(
            author_list(&names(3)),
            "Author1, A., Author2, A., & Author3, A."
        );
        let twenty_one = author_list(&names(21));
        assert!(twenty_one.contains("Author19, A., . . . Author21, A."));
        assert!(!twenty_one.contains("Author20"));
        assert_eq!(in_text_authors(&names(1)), "Author1");
        assert_eq!(in_text_authors(&names(2)), "Author1 & Author2");
        assert_eq!(in_text_authors(&names(3)), "Author1 et al.");
    }

    #[test]
    fn same_author_and_year_get_letters_in_list_order() {
        let refs = from_records(&[
            rec("2", &["Jane Smith"], Some(2021), "Zebra study"),
            rec("1", &["Jane Smith"], Some(2021), "Apple study"),
            rec("3", &["Ann Brown", "Bo Chen", "Cy Diaz"], None, "Third"),
        ]);
        assert_eq!(refs[0].cite, "Brown et al., n.d.");
        assert_eq!(refs[1].cite, "Smith, 2021a");
        assert_eq!(refs[1].record_id.as_deref(), Some("1"));
        assert_eq!(refs[2].narrative, "Smith (2021b)");
        assert_eq!(
            refs[1].plain(),
            "*Smith, J. (2021a). Apple study. Journal of Tests. https://doi.org/10.1/1"
        );
    }

    #[test]
    fn titles_in_apa_title_case() {
        assert_eq!(
            title_case("how do teachers in the UK use AI-based feedback: a systematic review"),
            "How Do Teachers in the UK Use AI-based Feedback: A Systematic Review"
        );
    }

    #[test]
    fn methods_references_render() {
        assert_eq!(
            thomas_harden().plain(),
            "Thomas, J., & Harden, A. (2008). Methods for the thematic synthesis of qualitative research in systematic reviews. BMC Medical Research Methodology, 8, Article 45. https://doi.org/10.1186/1471-2288-8-45"
        );
    }
}
