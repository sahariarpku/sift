//! Manual import of exported search results: RIS, BibTeX and PubMed NBIB (MEDLINE).
//! Formats follow the RIS tag specification (TY/ER, TI, AB, AU, PY, DO, JO/T2),
//! BibTeX `@type{key, field = {value}}`, and NLM MEDLINE tags (PMID, TI, AB, FAU/AU, DP, AID [doi], JT).

use super::adapter::{clean_doi, NormalisedRecord};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Ris,
    Bibtex,
    Nbib,
}

impl Format {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "ris" => Some(Format::Ris),
            "bib" | "bibtex" => Some(Format::Bibtex),
            "nbib" | "medline" => Some(Format::Nbib),
            _ => None,
        }
    }

    /// Guesses the format from the text.
    pub fn detect(text: &str) -> Option<Self> {
        let t = text.trim_start();
        if t.starts_with('@') {
            Some(Format::Bibtex)
        } else if t.starts_with("TY  -") {
            Some(Format::Ris)
        } else if t.starts_with("PMID-") {
            Some(Format::Nbib)
        } else {
            None
        }
    }
}

pub fn parse(format: Format, text: &str) -> Vec<NormalisedRecord> {
    match format {
        Format::Ris => parse_tagged(text, "ER", ris_field),
        Format::Nbib => parse_nbib(text),
        Format::Bibtex => parse_bibtex(text),
    }
}

fn year_from(s: &str) -> Option<i32> {
    s.split(|c: char| !c.is_ascii_digit())
        .find(|p| p.len() == 4)
        .and_then(|p| p.parse().ok())
}

fn ris_field(rec: &mut NormalisedRecord, tag: &str, value: &str) {
    match tag {
        "TI" | "T1" => rec.title = Some(value.to_owned()),
        "AB" | "N2" => rec.abstract_ = Some(value.to_owned()),
        "AU" | "A1" => rec.authors.push(value.to_owned()),
        "PY" | "Y1" | "DA" if rec.year.is_none() => rec.year = year_from(value),
        "DO" => rec.doi = clean_doi(value),
        "JO" | "JF" | "T2" if rec.venue.is_none() => rec.venue = Some(value.to_owned()),
        "UR" if rec.landing_url.is_none() => rec.landing_url = Some(value.to_owned()),
        "ID" | "AN" if rec.source_id.is_none() => rec.source_id = Some(value.to_owned()),
        _ => {}
    }
}

/// RIS: `XX  - value` lines, records end at `ER  -`.
fn parse_tagged(
    text: &str,
    end: &str,
    apply: fn(&mut NormalisedRecord, &str, &str),
) -> Vec<NormalisedRecord> {
    let mut out = Vec::new();
    let mut cur = NormalisedRecord::default();
    let mut any = false;
    for line in text.lines() {
        let line = line.trim_end();
        if line.len() < 5 || &line[2..5] != "  -" {
            continue;
        }
        let tag = &line[..2];
        let value = line[5..].trim();
        if tag == end {
            if any {
                out.push(std::mem::take(&mut cur));
            }
            any = false;
            continue;
        }
        if !value.is_empty() {
            apply(&mut cur, tag, value);
            any = true;
        }
    }
    if any {
        out.push(cur);
    }
    out
}

/// MEDLINE: 4-character tags padded with spaces, `- ` separator, continuation
/// lines indented by six spaces, blank line between records.
fn parse_nbib(text: &str) -> Vec<NormalisedRecord> {
    let mut out = Vec::new();
    for block in text.split("\n\n").filter(|b| b.contains("PMID-")) {
        let mut fields: Vec<(String, String)> = Vec::new();
        for line in block.lines() {
            if line.starts_with("      ") {
                if let Some(last) = fields.last_mut() {
                    last.1.push(' ');
                    last.1.push_str(line.trim());
                }
            } else if line.len() > 6 && &line[4..6] == "- " {
                fields.push((line[..4].trim().to_owned(), line[6..].trim().to_owned()));
            }
        }
        let mut r = NormalisedRecord::default();
        for (tag, v) in &fields {
            match tag.as_str() {
                "PMID" => r.source_id = Some(v.clone()),
                "TI" => r.title = Some(v.clone()),
                "AB" => r.abstract_ = Some(v.clone()),
                "FAU" => r.authors.push(v.clone()),
                "DP" if r.year.is_none() => r.year = year_from(v),
                "AID" if v.ends_with("[doi]") => r.doi = clean_doi(v.trim_end_matches("[doi]")),
                "JT" => r.venue = Some(v.clone()),
                _ => {}
            }
        }
        if r.authors.is_empty() {
            r.authors = fields
                .iter()
                .filter(|(t, _)| t == "AU")
                .map(|(_, v)| v.clone())
                .collect();
        }
        out.push(r);
    }
    out
}

/// A tolerant BibTeX reader: `field = {value}` or `field = "value"`, braces may nest.
fn parse_bibtex(text: &str) -> Vec<NormalisedRecord> {
    let mut out = Vec::new();
    for entry in text.split('@').skip(1) {
        let Some(open) = entry.find('{') else {
            continue;
        };
        let kind = entry[..open].trim().to_ascii_lowercase();
        if kind == "comment" || kind == "string" || kind == "preamble" {
            continue;
        }
        let body = &entry[open + 1..];
        let key_end = body.find(',').unwrap_or(body.len());
        let mut r = NormalisedRecord {
            source_id: Some(body[..key_end].trim().to_owned()),
            ..Default::default()
        };
        let mut rest = body.get(key_end + 1..).unwrap_or("");
        while let Some(eq) = rest.find('=') {
            let name = rest[..eq]
                .trim()
                .trim_start_matches(',')
                .trim()
                .to_ascii_lowercase();
            let after = rest[eq + 1..].trim_start();
            let (value, consumed) = if let Some(stripped) = after.strip_prefix('{') {
                let mut depth = 1;
                let mut end = 0;
                for (i, c) in stripped.char_indices() {
                    match c {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                end = i;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                (
                    stripped[..end].to_owned(),
                    &stripped[(end + 1).min(stripped.len())..],
                )
            } else if let Some(stripped) = after.strip_prefix('"') {
                let end = stripped.find('"').unwrap_or(stripped.len());
                (
                    stripped[..end].to_owned(),
                    &stripped[(end + 1).min(stripped.len())..],
                )
            } else {
                let end = after.find([',', '}']).unwrap_or(after.len());
                (after[..end].trim().to_owned(), &after[end..])
            };
            let value = value
                .replace(['{', '}'], "")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            match name.as_str() {
                "title" => r.title = Some(value),
                "abstract" => r.abstract_ = Some(value),
                "author" => r.authors = value.split(" and ").map(|a| a.trim().to_owned()).collect(),
                "year" => r.year = year_from(&value),
                "doi" => r.doi = clean_doi(&value),
                "journal" | "booktitle" => r.venue = Some(value),
                "url" => r.landing_url = Some(value),
                _ => {}
            }
            rest = consumed;
        }
        out.push(r);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ris() {
        let t = "TY  - JOUR\nTI  - Belonging abroad\nAU  - Li, Mei\nAU  - Smith, J\nPY  - 2019/03/01\nDO  - https://doi.org/10.1/ABC\nAB  - We interviewed students.\nJO  - Journal X\nER  - \nTY  - JOUR\nTI  - Second\nER  - \n";
        let r = parse(Format::Ris, t);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].authors.len(), 2);
        assert_eq!(r[0].year, Some(2019));
        assert_eq!(r[0].doi.as_deref(), Some("10.1/abc"));
        assert_eq!(Format::detect(t), Some(Format::Ris));
    }

    #[test]
    fn nbib() {
        let t = "PMID- 123\nTI  - Stress among Chinese students in the\n      United Kingdom.\nAB  - Abstract here.\nFAU - Wang, Li\nAU  - Wang L\nDP  - 2021 Jan\nAID - 10.2/xyz [doi]\nJT  - Some Journal\n\nPMID- 456\nTI  - Other\n";
        let r = parse(Format::Nbib, t);
        assert_eq!(r.len(), 2);
        assert_eq!(
            r[0].title.as_deref(),
            Some("Stress among Chinese students in the United Kingdom.")
        );
        assert_eq!(r[0].doi.as_deref(), Some("10.2/xyz"));
        assert_eq!(r[0].authors, vec!["Wang, Li".to_string()]);
    }

    #[test]
    fn bibtex() {
        let t = "@article{li2020,\n  title = {Being {Chinese} in the UK},\n  author = {Li, Mei and Brown, Sam},\n  year = 2020,\n  doi = \"10.3/Q\",\n  journal = {Studies}\n}\n@comment{ignore}";
        let r = parse(Format::Bibtex, t);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].title.as_deref(), Some("Being Chinese in the UK"));
        assert_eq!(r[0].authors.len(), 2);
        assert_eq!(r[0].year, Some(2020));
        assert_eq!(r[0].doi.as_deref(), Some("10.3/q"));
    }
}
