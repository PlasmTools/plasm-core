//! Parse-gate for explicitly tagged Plasm fences in published docs.
//!
//! Fence tags:
//! - `plasm` — catalog-independent [`parse_program_shape`]
//! - `plasm-matrix` — parse each non-empty line / whole program against
//!   `fixtures/schemas/plasm_language_matrix` via [`parse_with_cgs_layers_program`]
//! - `plasm-skip` — intentionally partial; body must start with a `# reason:` line
//!
//! Untagged ` ```text ` fences that look like executable Plasm with legacy `p#`
//! tokens fail the suite (full cutover).

use std::path::{Path, PathBuf};

use crate::expr_parser::{parse_program_shape, parse_with_cgs_layers_program};
use crate::loader::load_schema_dir;
use crate::symbol_tuning::TeachingExposureSession;
use crate::CgsLayer;

fn oss_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn language_matrix_dir() -> PathBuf {
    oss_root().join("fixtures/schemas/plasm_language_matrix")
}

#[derive(Debug)]
struct Fence<'a> {
    path: &'a Path,
    lang: &'a str,
    body: &'a str,
    line: usize,
}

fn extract_fences<'a>(path: &'a Path, text: &'a str) -> Vec<Fence<'a>> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0usize;
    let mut line = 1usize;
    while i < bytes.len() {
        if bytes[i] == b'\n' {
            line += 1;
            i += 1;
            continue;
        }
        if i + 3 <= bytes.len() && &bytes[i..i + 3] == b"```" {
            let fence_line = line;
            i += 3;
            let lang_start = i;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            let lang = std::str::from_utf8(&bytes[lang_start..i]).unwrap_or("");
            if i < bytes.len() && bytes[i] == b'\n' {
                line += 1;
                i += 1;
            }
            let body_start = i;
            while i + 3 <= bytes.len() {
                if &bytes[i..i + 3] == b"```" {
                    break;
                }
                if bytes[i] == b'\n' {
                    line += 1;
                }
                i += 1;
            }
            let body = std::str::from_utf8(&bytes[body_start..i])
                .unwrap_or("")
                .trim();
            if i + 3 <= bytes.len() {
                i += 3;
            }
            out.push(Fence {
                path,
                lang: lang.trim(),
                body,
                line: fence_line,
            });
            continue;
        }
        i += 1;
    }
    out
}

fn looks_like_plasm_with_legacy_p(body: &str) -> bool {
    let has_e = body.chars().any(|_| true)
        && (body.contains("e1")
            || body.contains("e2")
            || body.contains(".filter")
            || body.contains(".limit")
            || body.contains(" = e"));
    let has_p = body
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .any(|tok| {
            tok.len() >= 2 && tok.starts_with('p') && tok[1..].bytes().all(|b| b.is_ascii_digit())
        });
    has_e && has_p
}

fn doc_paths() -> Vec<PathBuf> {
    let root = oss_root().join("doc-site/docs");
    [
        "reference/plasm-language-definition.md",
        "reference/plasm-row-compute.md",
        "reference/plasm-long-operations.md",
        "reference/plasm-cgs-remote-terminal.md",
        "appliance/quickstart.md",
        "concepts.md",
        "authoring/index.md",
        "authoring/reference.md",
    ]
    .into_iter()
    .map(|rel| root.join(rel))
    .filter(|p| p.is_file())
    .collect()
}

#[test]
fn doc_fenced_plasm_examples_parse_under_language_matrix() {
    let matrix_dir = language_matrix_dir();
    let cgs = load_schema_dir(&matrix_dir).expect("load plasm_language_matrix");
    // Pin LangItem as e1 so published `plasm-matrix` fences stay stable.
    let exp = TeachingExposureSession::new(&cgs, "langmatrix", &["LangItem"]);
    let sym_map = exp.symbol_map_arc();
    let stack = [CgsLayer::new("langmatrix", &cgs)];

    let mut checked_shape = 0usize;
    let mut checked_matrix = 0usize;

    for path in doc_paths() {
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!("read {}: {e}", path.display());
        });
        for fence in extract_fences(&path, &text) {
            let loc = format!("{}:{}", fence.path.display(), fence.line);
            match fence.lang {
                "plasm-skip" => {
                    assert!(
                        fence
                            .body
                            .lines()
                            .next()
                            .is_some_and(|l| l.trim_start().starts_with("# reason:")),
                        "{loc}: plasm-skip fence must start with `# reason:`"
                    );
                }
                "plasm" => {
                    parse_program_shape(fence.body).unwrap_or_else(|e| {
                        panic!("{loc}: parse_program_shape failed: {e}\n{}", fence.body);
                    });
                    checked_shape += 1;
                }
                "plasm-matrix" => {
                    // Multi-line fences must be well-formed programs; each surface expression
                    // (whole line or binding RHS) must parse against the language matrix.
                    if fence.body.contains('\n') && fence.body.contains('=') {
                        parse_program_shape(fence.body).unwrap_or_else(|e| {
                            panic!("{loc}: shape failed: {e}\n{}", fence.body);
                        });
                    }
                    for line in fence.body.lines() {
                        let line = line.trim();
                        if line.is_empty() || line.starts_with("--") || line.starts_with('#') {
                            continue;
                        }
                        let expr = if let Some((label, rhs)) = line.split_once('=') {
                            let label = label.trim();
                            if label
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                            {
                                rhs.trim()
                            } else {
                                line
                            }
                        } else {
                            line
                        };
                        if expr.is_empty() {
                            continue;
                        }
                        parse_with_cgs_layers_program(expr, &stack, sym_map.clone(), None, false)
                            .unwrap_or_else(|e| {
                                panic!("{loc}: matrix parse failed on `{expr}`: {e}");
                            });
                    }
                    checked_matrix += 1;
                }
                "text" | "" => {
                    assert!(
                        !looks_like_plasm_with_legacy_p(fence.body),
                        "{loc}: untagged text fence still teaches legacy p#; retag as plasm/plasm-matrix or plasm-skip"
                    );
                }
                _ => {}
            }
        }
    }

    assert!(
        checked_shape + checked_matrix > 0,
        "expected at least one ```plasm or ```plasm-matrix fence under doc-site/docs"
    );
}
