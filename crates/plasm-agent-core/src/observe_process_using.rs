//! Observe-site process-using footer — **cut over** (identity).
//!
//! Formerly appended F_m0 didactic lines after plural identity TSV dumps. That was
//! harness/host reinterpretation of Plasm tool Markdown; agents must read Plasm
//! payloads as emitted. Ablation variants remain under AppWorld `ablation_offline/`.

/// Formerly F_m0 footer line; retained for ablation/offline imports — empty cutover.
#[allow(dead_code)] // retained for ablation/offline callers; live publish is identity
pub fn format_fm0_process_using_footer(
    _identity_headers: &[String],
    _headers: &[String],
) -> String {
    String::new()
}

/// Cutover: return Plasm Markdown unchanged (no didactic footers).
#[allow(dead_code)] // retained API; mcp_publish no longer calls this
pub fn append_tsv_process_using_footers(text: &str) -> String {
    text.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fm0_cutover_is_identity() {
        let mut paths = vec!["path\tflag".to_string()];
        for i in 1..=11 {
            paths.push(format!("/zone/work/item_{i:02}.dat\ttrue"));
        }
        let path_tsv = format!("## files (11 rows)\n```tsv\n{}\n```\n", paths.join("\n"));
        let out = append_tsv_process_using_footers(&path_tsv);
        assert_eq!(out, path_tsv);
        assert!(!out.contains("not N applies"));
    }

    #[test]
    fn skips_non_identity_small_dump() {
        let tsv = "## status (2 rows)\n```tsv\nstate\tok\npending\n```\n";
        let out = append_tsv_process_using_footers(tsv);
        assert_eq!(out, tsv);
    }
}
