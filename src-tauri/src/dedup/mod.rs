//! Deduplication (§8.3): exact DOI match first; then normalised title
//! Jaro-Winkler >= 0.95 with year within 1 and matching first-author surname.
//! Pairs from 0.90 to 0.95 are listed for a person to check.

use std::collections::HashMap;

use strsim::jaro_winkler;

#[derive(Debug, Clone)]
pub struct Candidate {
    pub id: String,
    pub doi: Option<String>,
    pub title: Option<String>,
    pub year: Option<i32>,
    pub first_author: Option<String>,
    /// Earlier-found records are kept; later ones are marked duplicates.
    pub order: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Merge {
    pub duplicate: String,
    pub of: String,
    pub rule: MergeRule,
    pub similarity: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeRule {
    Doi,
    TitleYearAuthor,
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub merges: Vec<Merge>,
    /// (a, b, similarity) pairs a person should check.
    pub borderline: Vec<(String, String, f64)>,
}

pub fn normalise_title(t: &str) -> String {
    let lower = t.to_lowercase();
    let cleaned: String = lower
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Surname from "Surname, Given", "Given Surname" or "Surname G".
pub fn surname(author: &str) -> String {
    let a = author.trim();
    let s = if let Some((last, _)) = a.split_once(',') {
        last
    } else {
        let parts: Vec<&str> = a.split_whitespace().collect();
        match parts.as_slice() {
            // "Wang L" (MEDLINE style): surname first, initials after.
            [last, init] if init.len() <= 2 && init.chars().all(|c| c.is_uppercase()) => last,
            [.., last] => last,
            [] => "",
        }
    };
    s.to_lowercase()
        .chars()
        .filter(|c| c.is_alphabetic())
        .collect()
}

pub fn find(mut records: Vec<Candidate>) -> Outcome {
    records.sort_by_key(|r| r.order);
    let mut out = Outcome::default();
    let mut removed: std::collections::HashSet<String> = Default::default();

    // 1. Exact DOI.
    let mut by_doi: HashMap<String, String> = HashMap::new();
    for r in &records {
        if let Some(d) = &r.doi {
            match by_doi.get(d) {
                Some(keep) => {
                    out.merges.push(Merge {
                        duplicate: r.id.clone(),
                        of: keep.clone(),
                        rule: MergeRule::Doi,
                        similarity: 1.0,
                    });
                    removed.insert(r.id.clone());
                }
                None => {
                    by_doi.insert(d.clone(), r.id.clone());
                }
            }
        }
    }

    // 2. Title + year + first author, blocked by year to keep it fast.
    let kept: Vec<(&Candidate, String)> = records
        .iter()
        .filter(|r| !removed.contains(&r.id))
        .filter_map(|r| r.title.as_deref().map(|t| (r, normalise_title(t))))
        .filter(|(_, t)| t.len() >= 10)
        .collect();
    let mut by_year: HashMap<i32, Vec<usize>> = HashMap::new();
    for (i, (r, _)) in kept.iter().enumerate() {
        by_year.entry(r.year.unwrap_or(0)).or_default().push(i);
    }
    for j in 0..kept.len() {
        let (b, tb) = &kept[j];
        if removed.contains(&b.id) {
            continue;
        }
        let y = b.year.unwrap_or(0);
        let mut best: Option<(usize, f64)> = None;
        for yy in [y - 1, y, y + 1] {
            for &i in by_year.get(&yy).map(Vec::as_slice).unwrap_or(&[]) {
                if i >= j || removed.contains(&kept[i].0.id) {
                    continue;
                }
                let sim = jaro_winkler(&kept[i].1, tb);
                if sim >= 0.90 && best.is_none_or(|(_, s)| sim > s) {
                    best = Some((i, sim));
                }
            }
        }
        let Some((i, sim)) = best else { continue };
        let a = kept[i].0;
        let same_author = match (&a.first_author, &b.first_author) {
            (Some(x), Some(y)) => surname(x) == surname(y) && !surname(x).is_empty(),
            _ => false,
        };
        let year_ok = matches!((a.year, b.year), (Some(p), Some(q)) if (p - q).abs() <= 1);
        if sim >= 0.95 && same_author && year_ok {
            out.merges.push(Merge {
                duplicate: b.id.clone(),
                of: a.id.clone(),
                rule: MergeRule::TitleYearAuthor,
                similarity: sim,
            });
            removed.insert(b.id.clone());
        } else {
            out.borderline.push((a.id.clone(), b.id.clone(), sim));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(
        id: &str,
        doi: Option<&str>,
        title: &str,
        year: i32,
        author: &str,
        order: i64,
    ) -> Candidate {
        Candidate {
            id: id.into(),
            doi: doi.map(str::to_owned),
            title: Some(title.into()),
            year: Some(year),
            first_author: Some(author.into()),
            order,
        }
    }

    #[test]
    fn doi_then_title_rules() {
        let out = find(vec![
            c(
                "a",
                Some("10.1/x"),
                "Belonging among Chinese students in UK universities",
                2019,
                "Li, Mei",
                1,
            ),
            c(
                "b",
                Some("10.1/x"),
                "Completely different title text",
                2019,
                "Other",
                2,
            ),
            c(
                "c",
                None,
                "Belonging among Chinese students in UK universities.",
                2020,
                "Mei Li",
                3,
            ),
            c(
                "d",
                None,
                "Belonging among Chinese students in UK universities",
                2012,
                "Li, Mei",
                4,
            ),
            c(
                "e",
                None,
                "Belonging among Chinese students in US universities",
                2019,
                "Zhang, Q",
                5,
            ),
        ]);
        let pairs: Vec<(&str, &str, MergeRule)> = out
            .merges
            .iter()
            .map(|m| (m.duplicate.as_str(), m.of.as_str(), m.rule))
            .collect();
        assert!(pairs.contains(&("b", "a", MergeRule::Doi)));
        assert!(
            pairs.contains(&("c", "a", MergeRule::TitleYearAuthor)),
            "{pairs:?}"
        );
        assert!(
            !pairs.iter().any(|p| p.0 == "d"),
            "year differs by more than 1"
        );
        assert!(!pairs.iter().any(|p| p.0 == "e"), "different author");
        assert!(out.borderline.iter().any(|(_, b, _)| b == "e"));
    }

    #[test]
    fn surnames() {
        assert_eq!(surname("Li, Mei"), "li");
        assert_eq!(surname("Mei Li"), "li");
        assert_eq!(surname("Wang L"), "wang");
        assert_eq!(
            normalise_title("Being: Chinese—in the UK!"),
            "being chinese in the uk"
        );
    }
}
