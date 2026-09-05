//! AppWorld teaching-table regressions (credential bootstrap + MutationResult login).

use crate::loader::load_schema_dir;
use crate::prompt_pipeline::PromptPipelineConfig;
use crate::symbol_tuning::TeachingExposureSession;

use super::{split_tsv_teaching_contract_and_table, validate_teaching_tsv_teaching_table};

fn apis_dir(name: &str) -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("..");
    p.push("..");
    p.push("apis");
    p.push(name);
    p
}

/// AppWorld AccountPassword — keyed Get with password field alphabet (not method invoke).
#[test]
fn appworld_account_password_teaches_keyed_get_not_method_invoke() {
    let dir = apis_dir("appworld/supervisor");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let exp = TeachingExposureSession::new(&cgs, "", &["AccountPassword"]);
    let body =
        PromptPipelineConfig::default().render_teaching_first_wave_for_session(&cgs, &exp, None);
    let (_, table) = split_tsv_teaching_contract_and_table(&body);
    validate_teaching_tsv_teaching_table(&table).expect("valid teaching rows");

    assert!(
        table.lines().any(|l| {
            let expr = l.split('\t').next().unwrap_or("").trim();
            expr.contains("{account_name=<wire>}")
                && (expr.contains("[password]")
                    || (expr.contains("password") && expr.contains('[')))
        }),
        "AccountPassword must teach keyed Get with password field alphabet:\n{table}"
    );
    assert!(
        table.lines().any(|l| {
            let expr = l.split('\t').next().unwrap_or("").trim();
            expr.contains("{account_name=<wire>}")
        }),
        "AccountPassword must teach e#{{account_name=<wire>}}:\n{table}"
    );
    let body_filled = body.replace("<wire>", "venmo");
    let line = body_filled
        .lines()
        .find(|l| l.contains("{account_name="))
        .expect("keyed line");
    let expr = line.split('\t').next().unwrap().trim();
    let parsed = crate::expr_parser::parse_session_line(expr, &cgs, Some(exp.symbol_map_arc()))
        .expect("parse keyed teaching");
    assert!(
        matches!(parsed.expr, crate::Expr::Get(_)),
        "keyed AccountPassword teaching must lower to Get, got {:?}",
        parsed.expr
    );
    assert!(
        !table.lines().any(|l| {
            let expr = l.split('\t').next().unwrap_or("");
            expr.contains(".m") && expr.ends_with("()") && expr.starts_with('e')
        }),
        "AccountPassword must not teach invalid e#.m#():\n{table}"
    );
}

/// Supervisor singleton profile must teach `[email,…]` on the bare `e#` fetch head.
#[test]
fn appworld_supervisor_singleton_teaches_email_field_alphabet() {
    let dir = apis_dir("appworld/supervisor");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let exp = TeachingExposureSession::new(&cgs, "", &["Supervisor"]);
    let body =
        PromptPipelineConfig::default().render_teaching_first_wave_for_session(&cgs, &exp, None);
    let (_, table) = split_tsv_teaching_contract_and_table(&body);
    validate_teaching_tsv_teaching_table(&table).expect("valid teaching rows");
    assert!(
        table.lines().any(|l| {
            let expr = l.split('\t').next().unwrap_or("").trim();
            expr.contains('[') && expr.contains("email")
        }),
        "Supervisor singleton must teach email in field alphabet:\n{table}"
    );
}

/// AuthSession login Meaning must teach `↠ e#[access_token,…]` with no `chain:` hint.
#[test]
fn appworld_venmo_login_teaches_mutation_result_field_alphabet() {
    let dir = apis_dir("appworld/venmo");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let exp = TeachingExposureSession::new(&cgs, "", &["AuthSession"]);
    let body =
        PromptPipelineConfig::default().render_teaching_first_wave_for_session(&cgs, &exp, None);
    let (_, table) = split_tsv_teaching_contract_and_table(&body);
    validate_teaching_tsv_teaching_table(&table).expect("valid teaching rows");
    let login_row = table
        .lines()
        .find(|l| {
            let expr = l.split('\t').next().unwrap_or("");
            expr.contains(".m") && (expr.contains("username") || expr.contains("password"))
        })
        .expect("login invoke teaching row");
    let meaning = login_row.split('\t').nth(1).unwrap_or("");
    assert!(
        meaning.contains('↠') && meaning.contains("access_token") && meaning.contains('['),
        "login Meaning must be ↠ e#[access_token,…]; got {meaning:?}\n{table}"
    );
    assert!(
        !meaning.contains("chain:"),
        "login Meaning must not emit chain: hint; got {meaning:?}"
    );
}
