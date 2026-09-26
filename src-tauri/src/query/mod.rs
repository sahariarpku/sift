//! Search query AST (§7). Built from the locked protocol's concepts; each
//! source adapter renders it into its own syntax and reports anything it had
//! to drop or approximate.

use serde::{Deserialize, Serialize};

use crate::agents::protocol::{ProtocolBody, ReviewOptions};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TermKind {
    Phrase,
    Word,
    Truncated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    TitleAbstract,
    Subject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Term {
    /// Without quotes or trailing `*`.
    pub text: String,
    pub kind: TermKind,
    pub fields: Vec<Field>,
    /// `("ERIC", descriptor)` for thesaurus terms.
    pub controlled_vocab: Option<(String, String)>,
}

impl Term {
    /// Classifies a protocol search term: `belong*` is truncated, `"x y"` a phrase.
    pub fn parse(raw: &str) -> Option<Term> {
        let t = raw.trim().trim_matches('"').trim();
        if t.is_empty() {
            return None;
        }
        let (text, truncated) = match t.strip_suffix('*') {
            Some(stem) => (stem.trim_end().to_owned(), true),
            None => (t.to_owned(), false),
        };
        let kind = if text.contains(char::is_whitespace) {
            TermKind::Phrase
        } else if truncated {
            TermKind::Truncated
        } else {
            TermKind::Word
        };
        // A truncated phrase ("British universit*") keeps its * as part of the phrase text.
        let text = if truncated && kind == TermKind::Phrase {
            format!("{text}*")
        } else {
            text
        };
        Some(Term {
            text,
            kind,
            fields: vec![Field::TitleAbstract],
            controlled_vocab: None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Concept {
    pub name: String,
    pub terms: Vec<Term>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Filters {
    pub year_from: Option<i32>,
    pub year_to: Option<i32>,
    pub languages: Vec<String>,
    pub doc_types: Vec<String>,
}

/// `And(Vec<Concept>)`, each concept `Or(Vec<Term>)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryAst {
    pub concepts: Vec<Concept>,
    pub filters: Filters,
    /// Terms that could not be represented (reported with every rendering).
    #[serde(default)]
    pub notes: Vec<String>,
}

/// Splits a protocol term on OR; a term with AND/NOT cannot sit inside an
/// OR group and is returned as an error message instead.
fn split_term(raw: &str) -> Result<Vec<String>, String> {
    let padded = format!(" {} ", raw.trim());
    if padded.contains(" AND ") || padded.contains(" NOT ") || raw.contains('(') {
        return Err(format!(
            "Term \"{}\" was dropped: it combines words with AND/NOT, which cannot sit inside an OR group. Put each part in its own concept.",
            raw.trim()
        ));
    }
    Ok(padded
        .split(" OR ")
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .collect())
}

impl QueryAst {
    pub fn from_protocol(p: &ProtocolBody, o: &ReviewOptions) -> Self {
        let mut notes = Vec::new();
        let concepts = p
            .concepts
            .iter()
            .map(|c| {
                let mut terms: Vec<Term> = Vec::new();
                for raw in &c.synonyms {
                    let parts = match split_term(raw) {
                        Ok(p) => p,
                        Err(note) => {
                            notes.push(note);
                            continue;
                        }
                    };
                    for s in parts {
                        if let Some(t) = Term::parse(&s) {
                            if !terms.iter().any(|x| x.text.eq_ignore_ascii_case(&t.text)) {
                                terms.push(t);
                            }
                        }
                    }
                }
                for v in &c.controlled_vocab {
                    let v = v.trim();
                    if !v.is_empty() {
                        terms.push(Term {
                            text: v.to_owned(),
                            kind: TermKind::Phrase,
                            fields: vec![Field::Subject],
                            controlled_vocab: Some(("ERIC".into(), v.to_owned())),
                        });
                    }
                }
                Concept {
                    name: c.name.clone(),
                    terms,
                }
            })
            .filter(|c| !c.terms.is_empty())
            .collect();
        QueryAst {
            concepts,
            filters: Filters {
                year_from: Some(o.year_from.max(p.date_range.from)),
                year_to: Some(o.year_to.min(p.date_range.to)),
                languages: p.languages.clone(),
                doc_types: Vec::new(),
            },
            notes,
        }
    }

    /// A narrower query for the seed set (§8.2): the first two free-text terms of each concept.
    pub fn narrow(&self) -> Self {
        let concepts = self
            .concepts
            .iter()
            .map(|c| Concept {
                name: c.name.clone(),
                terms: c
                    .terms
                    .iter()
                    .filter(|t| t.controlled_vocab.is_none())
                    .take(2)
                    .cloned()
                    .collect(),
            })
            .filter(|c| !c.terms.is_empty())
            .collect();
        QueryAst {
            concepts,
            filters: self.filters.clone(),
            notes: Vec::new(),
        }
    }

    /// Adds broadening terms to named concepts (seed loop). Returns how many were new.
    pub fn broaden(&mut self, additions: &[(String, Vec<String>)]) -> usize {
        let mut added = 0;
        for (name, terms) in additions {
            if let Some(c) = self
                .concepts
                .iter_mut()
                .find(|c| c.name.eq_ignore_ascii_case(name))
            {
                for raw in terms {
                    if let Some(t) = Term::parse(raw) {
                        if !c.terms.iter().any(|x| x.text.eq_ignore_ascii_case(&t.text)) {
                            c.terms.push(t);
                            added += 1;
                        }
                    }
                }
            }
        }
        added
    }
}

/// A query in one source's syntax, plus what it could not express.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Rendered {
    /// Human-readable query string (shown on the protocol screen and stored in `search_run`).
    pub query: String,
    /// Extra URL parameters (filters) the request needs.
    pub params: Vec<(String, String)>,
    pub simplifications: Vec<String>,
}

/// ISO 639-1 code for a language name, for sources that filter by code.
pub fn language_code(name: &str) -> Option<&'static str> {
    Some(match name.trim().to_ascii_lowercase().as_str() {
        "english" => "en",
        "chinese" => "zh",
        "spanish" => "es",
        "french" => "fr",
        "german" => "de",
        "portuguese" => "pt",
        "arabic" => "ar",
        "japanese" => "ja",
        "korean" => "ko",
        "italian" => "it",
        "russian" => "ru",
        "dutch" => "nl",
        "turkish" => "tr",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_terms() {
        assert_eq!(Term::parse("belong*").unwrap().kind, TermKind::Truncated);
        assert_eq!(Term::parse("belong*").unwrap().text, "belong");
        assert_eq!(
            Term::parse("\"international students\"").unwrap().kind,
            TermKind::Phrase
        );
        assert_eq!(Term::parse("UK").unwrap().kind, TermKind::Word);
        let p = Term::parse("British universit*").unwrap();
        assert_eq!(
            (p.kind, p.text.as_str()),
            (TermKind::Phrase, "British universit*")
        );
        assert!(Term::parse("  ").is_none());
    }

    #[test]
    fn boolean_operators_inside_terms_are_split_or_dropped() {
        let mut p = crate::agents::protocol::tests::sample_protocol();
        p.concepts[0].synonyms = vec![
            "Chinese student*".into(),
            "International student* AND China".into(),
            "Chinese undergraduates OR Chinese postgraduates".into(),
        ];
        let ast = QueryAst::from_protocol(&p, &ReviewOptions::default());
        let texts: Vec<&str> = ast.concepts[0]
            .terms
            .iter()
            .map(|t| t.text.as_str())
            .collect();
        assert!(
            texts.contains(&"Chinese undergraduates") && texts.contains(&"Chinese postgraduates")
        );
        assert!(!texts.iter().any(|t| t.contains("AND")));
        assert_eq!(ast.notes.len(), 1);
        assert!(ast.notes[0].contains("International student* AND China"));
    }

    #[test]
    fn builds_from_protocol_and_broadens() {
        let p = crate::agents::protocol::tests::sample_protocol();
        let mut ast = QueryAst::from_protocol(&p, &ReviewOptions::default());
        assert_eq!(ast.concepts.len(), 3);
        assert!(ast.concepts[0]
            .terms
            .iter()
            .any(|t| t.controlled_vocab.is_some()));
        assert_eq!(ast.filters.year_from, Some(2010));
        assert_eq!(ast.narrow().concepts[0].terms.len(), 2);
        let n = ast.broaden(&[(
            "experience".into(),
            vec!["lived experience".into(), "belong*".into()],
        )]);
        assert_eq!(n, 1, "duplicates are not added");
    }
}
