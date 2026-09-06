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

/// Spotify Player primary get must teach `e#{access_token=<wire>}` (RA-5), never song_id brace.
#[test]
fn appworld_spotify_player_teaches_access_token_not_song_id() {
    let dir = apis_dir("appworld/spotify");
    if !dir.exists() {
        return;
    }
    let cgs = load_schema_dir(&dir).unwrap();
    let exp = TeachingExposureSession::new(&cgs, "", &["Player", "LikedSong"]);
    let body =
        PromptPipelineConfig::default().render_teaching_first_wave_for_session(&cgs, &exp, None);
    let (_, table) = split_tsv_teaching_contract_and_table(&body);
    validate_teaching_tsv_teaching_table(&table).expect("valid teaching rows");

    let player_get = table.lines().find(|l| {
        let expr = l.split('\t').next().unwrap_or("").trim();
        expr.starts_with('e')
            && expr.contains("{access_token=<wire>}")
            && !expr.contains('.')
            && !expr.contains('~')
    });
    assert!(
        player_get.is_some(),
        "Player must teach e#{{access_token=<wire>}} primary get:\n{table}"
    );
    assert!(
        !table.lines().any(|l| {
            let expr = l.split('\t').next().unwrap_or("").trim();
            // Player must not use song_id as get identity; LikedSong{song_id=} keyed get is lawful.
            expr.starts_with('e')
                && expr.contains("{song_id=<wire>}")
                && !expr.contains('.')
                && l.contains("playback")
        }),
        "Player must not teach song_id brace as get identity:\n{table}"
    );
    assert!(
        table.lines().any(|l| {
            let expr = l.split('\t').next().unwrap_or("").trim();
            expr.contains("{access_token=<wire>}") && expr.contains("genre")
        }) || table.lines().any(|l| l.contains("genre")),
        "LikedSong teaching must surface genre:\n{table}"
    );
    assert!(
        table.lines().any(|l| {
            let expr = l.split('\t').next().unwrap_or("").trim();
            expr.contains("{access_token=<wire>}") && expr.contains("is_liked")
        }),
        "Player primary get must teach is_liked (liked-shelf membership):\n{table}"
    );

    let body_filled = body.replace("<wire>", "tok");
    let line = body_filled
        .lines()
        .find(|l| {
            let expr = l.split('\t').next().unwrap_or("").trim();
            expr.contains("{access_token=") && !expr.contains('.') && !expr.contains('~')
        })
        .expect("Player access_token get line");
    let expr = line.split('\t').next().unwrap().trim();
    // Strip projection bracket for parse if present
    let expr_core = expr.split('[').next().unwrap().trim();
    let parsed = crate::expr_parser::parse_session_line(expr_core, &cgs, Some(exp.symbol_map_arc()))
        .unwrap_or_else(|e| panic!("parse Player teaching {expr_core:?}: {e}"));
    assert!(
        matches!(parsed.expr, crate::Expr::Get(_)),
        "Player access_token teaching must lower to Get, got {:?}",
        parsed.expr
    );
}
