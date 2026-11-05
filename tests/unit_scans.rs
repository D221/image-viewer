//! Source-text and font-glyph scans: the fixtures the viewer draws must be
//! sound, and every character must exist in egui's default font.
//!
//! Split out of `tests/unit.rs`; the shared helpers live in
//! `tests/common/unit_fixtures.rs`.

#[path = "common/unit_fixtures.rs"]
mod unit_fixtures;

use unit_fixtures::*;

/// The three documents are structurally sound, and the damage this catches is
/// the kind a reader cannot report.
///
/// `README.md` carried **three** `# Image Viewer` headings, a stray keyboard row
/// at the top of the "Saving as" section, and the same zoom-preset row twice in
/// a row. All three came from edits landing at the wrong anchor - a line
/// *inserted* where a line was meant to be *replaced* - and none of them said
/// anything was wrong: the document still rendered, the keyboard row was still
/// correct in its proper place further down, and every check that looked for the
/// *content* passed.
///
/// So the checks are about **structure**, not content:
/// * one top-level title per document (the head corruption),
/// * no two identical adjacent lines (the pasted-once-too-often row),
/// * no one-row "table" (a lone `|` row is not a table at all, and the stray
///   rows were exactly that).
#[test]
fn each_document_is_structurally_sound() {
    for (name, text) in [
        ("README.md", include_str!("../README.md")),
        ("AGENTS.md", include_str!("../AGENTS.md")),
        ("TODO.md", include_str!("../TODO.md")),
    ] {
        let lines: Vec<&str> = text.lines().collect();

        let titles = lines.iter().filter(|l| l.starts_with("# ")).count();
        assert_eq!(titles, 1, "{name} has {titles} top-level titles");

        // Two identical adjacent lines are a paste that landed twice. Fenced
        // code is exempt: a sample of command output can legitimately repeat a
        // line.
        let mut in_fence = false;
        for pair in lines.windows(2) {
            let (a, b) = (pair[0].trim(), pair[1].trim());
            if a.starts_with("```") {
                in_fence = !in_fence;
                continue;
            }
            if in_fence || a.is_empty() || a != b {
                continue;
            }
            assert!(
                false,
                "{name}: two identical adjacent lines - {a:?} - which is an edit that landed twice"
            );
        }

        // A lone `|` row is not a table. The stray rows were exactly that: one
        // row, with prose above and below it.
        let mut i = 0;
        while i < lines.len() {
            if !lines[i].trim_start().starts_with('|') {
                i += 1;
                continue;
            }
            let start = i;
            while i < lines.len() && lines[i].trim_start().starts_with('|') {
                i += 1;
            }
            assert!(
                i - start > 1,
                "{name}: a single table row at line {} - {line:?} - is not a table",
                start + 1,
                line = lines[start].trim()
            );
        }
    }
}

/// The sources carry no duplicated comment line.
///
/// A comment line that appears twice in a row is an edit that landed at the wrong
/// anchor: the line was *inserted* where it was meant to *replace* another, and
/// the copy that was already there stayed. `src/main.rs` has carried
/// `// --- full-resolution tiles ---` twice since the zoom work, one of them at
/// column 0 in the middle of an indented block, which is the tell.
///
/// Two identical adjacent *code* lines are legitimate (`}` closing two things,
/// `0,` in an array), so the rule is about comments only.
#[test]
fn no_source_carries_a_duplicated_comment_line() {
    for (name, text) in all_src_files() {
        let lines: Vec<&str> = text.lines().collect();
        for (i, pair) in lines.windows(2).enumerate() {
            let (a, b) = (pair[0].trim(), pair[1].trim());
            if a.starts_with("//") && a == b {
                assert!(
                    false,
                    "{name}: the comment {a:?} appears twice in a row (line {}) - \
                     an edit that landed at the wrong anchor",
                    i + 1
                );
            }
        }
    }
}

#[test]
fn no_ui_text_uses_a_glyph_the_font_lacks() {
    let mut missing: Vec<(char, String)> = Vec::new();

    let ctx = egui::Context::default();
    // `run_pass`, not `run_ui`: this is a headless pass and nothing applies the
    // font-atlas delta, which epaint 0.36 debug-asserts on when it is dropped.
    let _ = run_pass(&ctx, egui::RawInput::default(), |ui| {
        // Two sizes: a glyph can be in the family and still be dropped when
        // rasterised at a size the atlas does not cover.
        let sizes = [14.0f32, 12.0];

        let mut seen: std::collections::BTreeMap<char, String> = Default::default();
        for file in ["src/main.rs", "src/lib.rs"] {
            let text =
                std::fs::read_to_string(file).unwrap_or_else(|e| panic!("cannot read {file}: {e}"));
            for (i, line) in text.lines().enumerate() {
                let code = strip_line_comment(line);
                for c in code.chars() {
                    if c.is_ascii() || c == '\u{FFFD}' {
                        continue;
                    }
                    seen.entry(c).or_insert_with(|| format!("{file}:{}", i + 1));
                }
            }
        }

        ui.ctx().fonts_mut(|f| {
            for (c, origin) in &seen {
                let s = c.to_string();
                let absent =
                    sizes.iter().any(|sz| !f.has_glyphs(&egui::FontId::proportional(*sz), &s));
                if absent {
                    missing.push((*c, origin.clone()));
                }
            }
        });
    });

    assert!(
        missing.is_empty(),
        "these characters are not in egui's default font and will render as blank \
         boxes:\n{}\nUse ASCII, or a character the font actually has (the app uses \
         '…' '—' '·' '°' '⟲' '⟳' successfully).",
        missing
            .iter()
            .map(|(c, o)| format!("  U+{:04X} '{}' first at {o}", *c as u32, c))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
