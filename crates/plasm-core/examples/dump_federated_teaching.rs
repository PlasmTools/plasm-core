//! Dump federated AppWorld gmail+venmo teaching TSV for offline ablations.
//!
//! ```text
//! cargo run -p plasm-core --example dump_federated_teaching -- \
//!   /path/to/teaching_rebuild/assets
//! ```

use indexmap::IndexMap;
use plasm_core::loader::load_schema_dir_unvalidated;
use plasm_core::symbol_tuning::TeachingExposureSession;
use plasm_core::{PromptPipelineConfig, CGS};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn apis_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/appworld")
}

fn load_bound(name: &str) -> CGS {
    let dir = apis_root().join(name);
    // Offline dump only — AppWorld catalogs may fail full teaching-coverage validation;
    // we still need production-shaped TSV from the loaded CGS graph.
    let mut cgs = load_schema_dir_unvalidated(&dir).unwrap_or_else(|e| panic!("load {name}: {e}"));
    cgs.bind_registry_entry_id(name);
    cgs
}

fn main() {
    let out = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../../scripts/appworld/cuga/ablation_offline/teaching_rebuild/assets")
        });
    std::fs::create_dir_all(&out).expect("mkdir assets");

    let gmail = load_bound("gmail");
    let venmo = load_bound("venmo");
    let gmail_arc = Arc::new(gmail.clone());
    let venmo_arc = Arc::new(venmo.clone());

    // Full non-abstract entity exposure per catalog (production federated card shape).
    // Seed list in dump_stats remains the ablation-relevant primaries for case authoring.
    let gmail_primaries = ["AuthSession", "EmailThread", "Email", "Draft", "Profile"];
    let venmo_primaries = [
        "AuthSession",
        "PaymentRequest",
        "Transaction",
        "User",
        "Friend",
    ];
    let mut gmail_all: Vec<&str> = gmail
        .entities
        .iter()
        .filter(|(_, e)| !e.abstract_entity)
        .map(|(n, _)| n.as_str())
        .collect();
    gmail_all.sort();
    let mut venmo_all: Vec<&str> = venmo
        .entities
        .iter()
        .filter(|(_, e)| !e.abstract_entity)
        .map(|(n, _)| n.as_str())
        .collect();
    venmo_all.sort();

    let layers = [gmail_arc.as_ref(), venmo_arc.as_ref()];
    let mut exp = TeachingExposureSession::new(gmail_arc.as_ref(), "gmail", &gmail_all);
    exp.expose_entities(&layers, venmo_arc.clone(), "venmo", &venmo_all);

    let mut by_entry: IndexMap<String, &CGS> = IndexMap::new();
    by_entry.insert("gmail".into(), gmail_arc.as_ref());
    by_entry.insert("venmo".into(), venmo_arc.as_ref());

    let pipeline = PromptPipelineConfig::default();
    let production =
        pipeline.render_teaching_first_wave_for_session_federated(&by_entry, &exp, None);
    write(&out.join("production_federated.tsv"), &production);

    // Live AppWorld first-wave arrival: Venmo primary + supervisor creds (not gmail).
    let supervisor = load_bound("supervisor");
    let supervisor_arc = Arc::new(supervisor);
    let venmo_arrival = ["PaymentRequest", "AuthSession", "Friend"];
    let supervisor_arrival = ["AccountPassword", "Supervisor"];
    let mut arrival_exp = TeachingExposureSession::new(venmo_arc.as_ref(), "venmo", &venmo_arrival);
    arrival_exp.expose_entities(
        &[venmo_arc.as_ref(), supervisor_arc.as_ref()],
        supervisor_arc.clone(),
        "supervisor",
        &supervisor_arrival,
    );
    let mut arrival_by_entry: IndexMap<String, &CGS> = IndexMap::new();
    arrival_by_entry.insert("venmo".into(), venmo_arc.as_ref());
    arrival_by_entry.insert("supervisor".into(), supervisor_arc.as_ref());
    let arrival = pipeline.render_teaching_first_wave_for_session_federated(
        &arrival_by_entry,
        &arrival_exp,
        None,
    );
    write(&out.join("venmo_supervisor_arrival.tsv"), &arrival);

    let inventory = derive_inventory(&production);
    write(&out.join("inventory.tsv"), &inventory);

    let minimal = derive_minimal(&production, &exp, &venmo);
    write(&out.join("minimal_tsv.tsv"), &minimal);

    let sql = extract_sql_grammar();
    write(&out.join("sql_grammar.txt"), &sql);

    let map = symbol_summary(&exp);
    write(
        &out.join("symbol_map.json"),
        &serde_json::to_string_pretty(&map).unwrap(),
    );

    let stats = serde_json::json!({
        "asset_dump_version": "tr_v1_gmail_venmo",
        "gmail_primaries": gmail_primaries,
        "venmo_primaries": venmo_primaries,
        "gmail_entities": gmail_all,
        "venmo_entities": venmo_all,
        "production_bytes": production.len(),
        "inventory_bytes": inventory.len(),
        "minimal_bytes": minimal.len(),
        "sql_grammar_bytes": sql.len(),
        "entity_count": exp.entities.len(),
    });
    write(
        &out.join("dump_stats.json"),
        &serde_json::to_string_pretty(&stats).unwrap(),
    );

    eprintln!(
        "wrote {} (prod {} B, inv {} B, min {} B, entities {})",
        out.display(),
        production.len(),
        inventory.len(),
        minimal.len(),
        exp.entities.len()
    );
}

fn write(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

fn derive_inventory(tsv: &str) -> String {
    let mut out = String::from("plasm_expr\tMeaning\n");
    for line in tsv.lines() {
        if line.starts_with("plasm_expr") || line.is_empty() {
            continue;
        }
        let Some((expr, meaning)) = line.split_once('\t') else {
            continue;
        };
        let e = expr.trim();
        // Keep entity heads, v# gloss, wire gloss rows — drop executable exemplars with ( { ~ . [
        let is_gloss = e.starts_with('v')
            || (!e.contains('(')
                && !e.contains('{')
                && !e.contains('~')
                && !e.contains('.')
                && !e.contains('['));
        let is_entity_banner =
            e.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && meaning.contains('·');
        if is_gloss
            || (e.starts_with('e') && e.chars().skip(1).all(|c| c.is_ascii_digit()))
            || (is_entity_banner && !e.contains('('))
        {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn derive_minimal(tsv: &str, exp: &TeachingExposureSession, venmo: &CGS) -> String {
    let mut out = String::from("plasm_expr\tMeaning\n");
    let mut seen_kinds: std::collections::HashSet<String> = std::collections::HashSet::new();
    for line in tsv.lines() {
        if line.starts_with("plasm_expr") || line.is_empty() {
            continue;
        }
        let Some((expr, meaning)) = line.split_once('\t') else {
            continue;
        };
        let e = expr.trim();
        // Always keep v# enum/token rows (HB inheritance).
        if e.starts_with('v') {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        // Keep wire→v# gloss links (tokens-first enum pedagogy).
        if !e.contains('(')
            && !e.contains('{')
            && !e.contains('~')
            && !e.contains('[')
            && meaning.contains('v')
            && (e.contains('_') || e.chars().all(|c| c.is_ascii_alphanumeric()))
        {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        let kind = if e.contains('~') {
            "search"
        } else if e.contains('{') && e.contains('}') {
            "query"
        } else if e.contains('(') && e.contains(')') && !e.contains('.') {
            "get"
        } else if e.contains(".r") || (e.contains('.') && meaning.contains("relation")) {
            "relation"
        } else if e.contains('[') {
            "project"
        } else if e.contains(".filter") {
            "filter"
        } else if e.starts_with('e') && e.chars().skip(1).all(|c| c.is_ascii_digit()) {
            "entity"
        } else if e.contains(".m") || (e.contains('.') && meaning.contains('↠')) {
            "method"
        } else {
            continue;
        };
        // One exemplar per (entity_prefix, kind) for minimal card.
        let ent = e
            .split(|c: char| !c.is_ascii_alphanumeric())
            .next()
            .unwrap_or(e);
        let key = format!("{ent}:{kind}");
        if seen_kinds.insert(key) {
            out.push_str(line);
            out.push('\n');
        }
    }
    // HB inheritance: tokens-first enum membership holes from CGS values:.
    append_enum_membership_holes(&mut out, exp, venmo);
    out
}

fn enum_tokens(cgs: &CGS, value_key: &str) -> Vec<String> {
    cgs.values
        .get(value_key)
        .and_then(|v| v.allowed_values.clone())
        .unwrap_or_default()
}

fn append_enum_membership_holes(out: &mut String, exp: &TeachingExposureSession, venmo: &CGS) {
    let targets = [
        ("venmo", "PaymentRequest", "status", "nv_status"),
        ("venmo", "Transaction", "direction", "nv_direction"),
    ];
    for (entry, entity, wire, value_key) in targets {
        let Some(idx) = exp
            .entities
            .iter()
            .zip(exp.entity_catalog_entry_ids.iter())
            .position(|(e, c)| e == entity && c.as_str() == entry)
        else {
            continue;
        };
        let e_sym = format!("e{}", idx + 1);
        let tokens = enum_tokens(venmo, value_key);
        if tokens.is_empty() {
            continue;
        }
        let membership = tokens.join("|");
        let v_sym = format!("v_enum_{wire}");
        let v_row = format!("{v_sym}\tenum · {wire} tokens: {}\n", tokens.join(", "));
        if !out.contains(&format!("{v_sym}\t")) {
            out.push_str(&v_row);
        }
        let wire_link = format!("{wire}\t{v_sym}\n");
        if !out.contains(&wire_link) {
            out.push_str(&wire_link);
        }
        let hole =
            format!("{e_sym}{{{wire}=<{membership}>}}\t↣ [{e_sym}] · membership hole ({wire})\n");
        if !out.contains(&format!("{e_sym}{{{wire}=")) {
            out.push_str(&hole);
        }
    }
}

fn extract_sql_grammar() -> String {
    let tool = include_str!("../src/prompt_render/assets/plasm_tool.txt");
    let mut out = String::new();
    let mut take = false;
    for line in tool.lines() {
        if line.starts_with("Core surface:") || line.starts_with("**Batch independent reads**") {
            take = true;
        }
        if take {
            out.push_str(line);
            out.push('\n');
        }
        // Stop before composition / symbol essays — Core surface signatures only.
        if take && line.starts_with("Composition rules:") {
            break;
        }
    }
    if out.is_empty() {
        out.push_str(
            "Core surface: Get e#(<id>); Search e#~\"q\"; Bare e# when → e; row-ops on ℓ after bind.\n",
        );
    }
    out
}

fn symbol_summary(exp: &TeachingExposureSession) -> serde_json::Value {
    let entities: Vec<_> = exp
        .entities
        .iter()
        .zip(exp.entity_catalog_entry_ids.iter())
        .enumerate()
        .map(|(i, (entity, entry_id))| {
            serde_json::json!({
                "e": format!("e{}", i + 1),
                "entity": entity,
                "entry_id": entry_id,
            })
        })
        .collect();
    serde_json::json!({
        "asset_dump_version": "tr_v1_gmail_venmo",
        "entities": entities
    })
}
