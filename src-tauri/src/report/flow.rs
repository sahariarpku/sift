//! PRISMA 2020 flow diagram as SVG, drawn from `v_prisma_counts` only.
//! Layout follows the PRISMA 2020 template for new systematic reviews that
//! include searches of databases and other methods (citation searching).

use crate::records::Prisma;

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn fmt_n(n: i64) -> String {
    let s = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

fn source_name(id: &str) -> &str {
    match id {
        "eric" => "ERIC",
        "openalex" => "OpenAlex",
        "core" => "CORE",
        "semantic_scholar" => "Semantic Scholar",
        "citation_chasing" => "Citation searching",
        "manual_import" => "Imported records",
        other => other,
    }
}

struct Box_ {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    lines: Vec<String>,
    shaded: bool,
}

fn draw(b: &Box_, out: &mut String) {
    let fill = if b.shaded { "#E3EEEC" } else { "#FFFFFF" };
    out.push_str(&format!(
        r##"<rect x="{}" y="{}" width="{}" height="{}" rx="6" fill="{fill}" stroke="#1C1B19" stroke-width="1.2"/>"##,
        b.x, b.y, b.w, b.h
    ));
    for (i, line) in b.lines.iter().enumerate() {
        let weight = if i == 0 { "600" } else { "400" };
        out.push_str(&format!(
            r##"<text x="{}" y="{}" font-family="IBM Plex Sans, Arial, sans-serif" font-size="13" font-weight="{weight}" fill="#1C1B19">{}</text>"##,
            b.x + 12,
            b.y + 22 + i as i32 * 18,
            esc(line)
        ));
    }
}

fn arrow(x1: i32, y1: i32, x2: i32, y2: i32, out: &mut String) {
    out.push_str(&format!(
        r##"<line x1="{x1}" y1="{y1}" x2="{x2}" y2="{y2}" stroke="#1C1B19" stroke-width="1.2" marker-end="url(#arrow)"/>"##
    ));
}

/// The diagram as a standalone SVG document.
pub fn svg(p: &Prisma) -> String {
    let (lx, rx, w, rw) = (20, 420, 360, 330);
    let db_sources: Vec<String> = p
        .by_source
        .iter()
        .filter(|(s, _)| s != "citation_chasing" && s != "manual_import")
        .map(|(s, n)| format!("{} (n = {})", source_name(s), fmt_n(*n)))
        .collect();
    let other: Vec<String> = p
        .by_source
        .iter()
        .filter(|(s, _)| s == "citation_chasing" || s == "manual_import")
        .map(|(s, n)| format!("{} (n = {})", source_name(s), fmt_n(*n)))
        .collect();
    let mut id_lines = vec![format!(
        "Records identified (n = {})",
        fmt_n(p.identified_databases + p.identified_other)
    )];
    id_lines.extend(db_sources);
    id_lines.extend(other);
    let id_h = 30 + 18 * id_lines.len() as i32;
    let mut reasons = vec![format!(
        "Reports excluded (n = {})",
        fmt_n(p.reports_excluded)
    )];
    reasons.extend(
        p.exclusion_reasons
            .iter()
            .map(|(r, n)| format!("{r} (n = {})", fmt_n(*n))),
    );
    if p.reports_awaiting > 0 {
        reasons.push(format!(
            "Awaiting classification, screening stopped (n = {})",
            fmt_n(p.reports_awaiting)
        ));
    }
    let reasons_h = 30 + 18 * reasons.len() as i32;
    let mut removed = vec![format!(
        "Duplicates removed (n = {})",
        fmt_n(p.duplicates_removed)
    )];
    if p.records_not_screened > 0 {
        removed.push(format!(
            "Not screened, screening stopped (n = {})",
            fmt_n(p.records_not_screened)
        ));
    }
    let removed_h = 30 + 18 * removed.len() as i32;

    let mut y = 50;
    let mut boxes = Vec::new();
    let mut arrows = Vec::new();
    let identified = Box_ {
        x: lx,
        y,
        w,
        h: id_h,
        lines: id_lines,
        shaded: false,
    };
    boxes.push(Box_ {
        x: rx,
        y,
        w: rw,
        h: removed_h,
        lines: removed,
        shaded: false,
    });
    arrows.push((lx + w, y + 24, rx, y + 24));
    y += id_h.max(removed_h) + 40;
    arrows.push((lx + w / 2, identified.y + identified.h, lx + w / 2, y));
    boxes.push(identified);

    let rows: Vec<(String, String, i32)> = vec![
        (
            format!("Records screened (n = {})", fmt_n(p.records_screened)),
            format!("Records excluded (n = {})", fmt_n(p.records_excluded)),
            48,
        ),
        (
            format!(
                "Reports sought for retrieval (n = {})",
                fmt_n(p.reports_sought)
            ),
            format!(
                "Reports not retrieved (n = {})",
                fmt_n(p.reports_not_retrieved)
            ),
            48,
        ),
        (
            format!(
                "Reports assessed for eligibility (n = {})",
                fmt_n(p.reports_assessed)
            ),
            String::new(),
            reasons_h.max(48),
        ),
    ];
    for (left, right, h) in rows {
        boxes.push(Box_ {
            x: lx,
            y,
            w,
            h: 48,
            lines: vec![left],
            shaded: false,
        });
        if right.is_empty() {
            boxes.push(Box_ {
                x: rx,
                y,
                w: rw,
                h,
                lines: reasons.clone(),
                shaded: false,
            });
        } else {
            boxes.push(Box_ {
                x: rx,
                y,
                w: rw,
                h: 48,
                lines: vec![right],
                shaded: false,
            });
        }
        arrows.push((lx + w, y + 24, rx, y + 24));
        let next = y + h.max(48) + 40;
        arrows.push((lx + w / 2, y + 48, lx + w / 2, next));
        y = next;
    }
    boxes.push(Box_ {
        x: lx,
        y,
        w,
        h: 66,
        lines: vec![
            format!(
                "Studies included in review (n = {})",
                fmt_n(p.studies_included)
            ),
            format!(
                "Reports of included studies (n = {})",
                fmt_n(p.reports_included)
            ),
        ],
        shaded: true,
    });
    let height = y + 90;
    let mut out = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="780" height="{height}" viewBox="0 0 780 {height}" role="img" aria-label="PRISMA 2020 flow diagram"><defs><marker id="arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto"><path d="M0,0 L10,5 L0,10 z" fill="#1C1B19"/></marker></defs><rect width="780" height="{height}" fill="#FFFFFF"/><text x="20" y="30" font-family="IBM Plex Sans, Arial, sans-serif" font-size="15" font-weight="600" fill="#1C1B19">PRISMA 2020 flow diagram</text>"##
    );
    for (x1, y1, x2, y2) in arrows {
        arrow(x1, y1, x2, y2, &mut out);
    }
    for b in &boxes {
        draw(b, &mut out);
    }
    out.push_str("</svg>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagram_shows_every_count() {
        let p = Prisma {
            identified_databases: 6332,
            identified_other: 12,
            by_source: vec![
                ("openalex".into(), 3902),
                ("eric".into(), 1284),
                ("citation_chasing".into(), 12),
            ],
            duplicates_removed: 1871,
            records_screened: 4461,
            records_not_screened: 0,
            records_excluded: 4152,
            reports_sought: 309,
            reports_not_retrieved: 23,
            reports_assessed: 286,
            reports_excluded: 244,
            reports_awaiting: 0,
            exclusion_reasons: vec![("EX-POP".into(), 97), ("EX-DESIGN".into(), 147)],
            studies_included: 38,
            reports_included: 42,
        };
        let s = svg(&p);
        if let Ok(out) = std::env::var("SIFT_WRITE_FLOW_SVG") {
            std::fs::write(out, &s).expect("write sample svg");
        }
        for needle in [
            "Records identified (n = 6,344)",
            "ERIC (n = 1,284)",
            "Citation searching (n = 12)",
            "Duplicates removed (n = 1,871)",
            "Records screened (n = 4,461)",
            "Records excluded (n = 4,152)",
            "Reports sought for retrieval (n = 309)",
            "Reports not retrieved (n = 23)",
            "Reports assessed for eligibility (n = 286)",
            "Reports excluded (n = 244)",
            "EX-POP (n = 97)",
            "Studies included in review (n = 38)",
            "Reports of included studies (n = 42)",
        ] {
            assert!(s.contains(needle), "{needle}");
        }
        assert!(s.starts_with("<svg") && s.ends_with("</svg>"));
        assert!(!s.contains("stopped"));
    }

    #[test]
    fn diagram_discloses_stopped_stages() {
        let p = Prisma {
            records_not_screened: 120,
            reports_awaiting: 7,
            ..Default::default()
        };
        let s = svg(&p);
        assert!(s.contains("Not screened, screening stopped (n = 120)"));
        assert!(s.contains("Awaiting classification, screening stopped (n = 7)"));
    }
}
