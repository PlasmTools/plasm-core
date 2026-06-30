//! GitHub symbol-resolution regressions (projection + cap-qualified mutator args).

use super::test_support::{
    compile_github_program, github_cgs, github_issue_label_session, github_ranked_mutator_session,
    github_symbol_map,
};
use crate::plasm_dag::compile_plasm_dag_to_plan;
use crate::plasm_plan_run::evaluate_plasm_plan_dry;

/// Unknown `p#` on Label projection surfaces an honest error (no Issue `body` phantom).
#[test]
fn label_projection_unknown_p_sym_is_typed_error_not_phantom_body() {
    let session = github_issue_label_session();
    let map = github_symbol_map(&session);
    let label_e = map.entity_sym_for("github", "Label");
    let repo_e = map.entity_sym_for("github", "Repository");
    let repo_owner = map.ident_sym_entity_field("Repository", "owner");
    let repo_name = map.ident_sym_entity_field("Repository", "repo");
    let issue_p_body = map.ident_sym_entity_field("Issue", "body");
    let source = format!(
        r#"repo = {repo_e}({repo_owner}="o", {repo_name}="r")
labels = {label_e}{{{p_repository}=repo.full_name}}
labels[{issue_p_body}]"#,
        repo_e = repo_e,
        repo_owner = repo_owner,
        repo_name = repo_name,
        label_e = label_e,
        p_repository = map.ident_sym_cap_param_for("github", "Label", "label_query", "repository"),
        issue_p_body = issue_p_body,
    );
    let err = compile_plasm_dag_to_plan(
        &plasm_core::PromptPipelineConfig::default(),
        None,
        &session,
        "label-bad-p",
        &source,
    )
    .expect_err("Issue body p# must not project Label rows");
    let msg = err.to_string();
    assert!(
        msg.contains("not a row symbol") || msg.contains("not a row field"),
        "expected typed symbol error, got {msg}"
    );
    assert!(
        !msg.contains("body") || msg.contains("not a row"),
        "must not treat Issue body as Label row field: {msg}"
    );
}

/// Label query `[p#,…]` projection must resolve tokens against the Label row contract, not
/// globally homographed Issue field wires (`state`, `issue_type_color`, …).
#[test]
fn label_query_projection_resolves_entity_scoped_p_symbols() {
    let session = github_issue_label_session();
    let map = github_symbol_map(&session);
    let p_name = map.ident_sym_entity_field("Label", "name");
    let p_color = map.ident_sym_entity_field("Label", "color");
    let p_desc = map.ident_sym_entity_field("Label", "description");
    let label_e = map.entity_sym_for("github", "Label");
    let repo_e = map.entity_sym_for("github", "Repository");
    let repo_owner = map.ident_sym_entity_field("Repository", "owner");
    let repo_name = map.ident_sym_entity_field("Repository", "repo");
    let p_repository = map.ident_sym_cap_param_for("github", "Label", "label_query", "repository");
    let source = format!(
        r#"repo = {repo_e}({repo_owner}="ryan-s-roberts", {repo_name}="tool-test")
labels = {label_e}{{{p_repository}=repo.full_name}}
labels[{p_name},{p_color},{p_desc}]"#,
        repo_e = repo_e,
        repo_owner = repo_owner,
        repo_name = repo_name,
        label_e = label_e,
        p_repository = p_repository,
        p_name = p_name,
        p_color = p_color,
        p_desc = p_desc,
    );
    let plan = compile_github_program(&session, "github-label-projection", &source);
    let return_id = plan["return"]["node"].as_str().expect("return node id");
    let return_node = plan["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .find(|n| n["id"] == return_id)
        .expect("return node");
    assert_eq!(return_node["kind"], "compute");
    let fields = return_node["compute"]["op"]["fields"]
        .as_object()
        .expect("project fields");
    assert!(
        fields.contains_key("name")
            && fields.contains_key("color")
            && fields.contains_key("description"),
        "expected Label wire columns, got {fields:?}"
    );
    assert!(
        !fields.contains_key("state") && !fields.contains_key("issue_type_color"),
        "must not project Issue homograph wires onto Label rows: {fields:?}"
    );
    evaluate_plasm_plan_dry(&session, &plan).expect("label projection dry-run");
}

/// `issue_create` dotted-call args must resolve cap-qualified `p#` for homograph params (`title`, `body`).
#[test]
fn issue_create_dry_run_resolves_cap_qualified_param_symbols() {
    let cgs = github_cgs();
    let session = github_ranked_mutator_session(
        &cgs,
        &["Repository", "Issue"],
        "create a new issue with title and body in the repository",
        &["issue_create"],
        "issue_create",
    );
    let map = github_symbol_map(&session);
    let issue_e = map.entity_sym_for("github", "Issue");
    let cap = cgs.get_capability("issue_create").expect("issue_create");
    let method_sym = map.method_sym("Issue", cap.name.as_str());
    let p_repo = map.ident_sym_cap_param_for("github", "Issue", "issue_create", "repository");
    let p_title = map.ident_sym_cap_param_for("github", "Issue", "issue_create", "title");
    let p_body = map.ident_sym_cap_param_for("github", "Issue", "issue_create", "body");
    let repo_e = map.entity_sym_for("github", "Repository");
    let repo_owner = map.ident_sym_entity_field("Repository", "owner");
    let repo_name = map.ident_sym_entity_field("Repository", "repo");
    let source = format!(
        r#"repo = {repo_e}({repo_owner}="ryan-s-roberts", {repo_name}="tool-test")
created = {issue_e}.{method_sym}({p_repo}=repo.full_name, {p_title}="Document labels", {p_body}="guide body")
created"#,
        repo_e = repo_e,
        repo_owner = repo_owner,
        repo_name = repo_name,
        issue_e = issue_e,
        method_sym = method_sym,
        p_repo = p_repo,
        p_title = p_title,
        p_body = p_body,
    );
    let plan = compile_github_program(&session, "github-issue-create", &source);
    let created = plan["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .find(|n| n["id"] == "created")
        .expect("created node");
    assert_eq!(created["kind"], "create");
    let ir_blob = created
        .get("ir")
        .filter(|v| !v.is_null())
        .or_else(|| created.get("ir_template"))
        .expect("plan IR or ir_template");
    assert!(
        ir_blob.to_string().contains("issue_create"),
        "expected issue_create in plan IR, got {ir_blob}"
    );
    evaluate_plasm_plan_dry(&session, &plan).expect("issue_create dry-run");
}

#[test]
fn issue_create_ir_input_uses_logical_param_names_not_opaque_p_symbols() {
    let cgs = github_cgs();
    let session = github_ranked_mutator_session(
        &cgs,
        &["Repository", "Issue"],
        "create a new issue with title and body in the repository",
        &["issue_create"],
        "issue_create",
    );
    let map = github_symbol_map(&session);
    let issue_e = map.entity_sym_for("github", "Issue");
    let cap = cgs.get_capability("issue_create").expect("issue_create");
    let method_sym = map.method_sym("Issue", cap.name.as_str());
    let p_repo = map.ident_sym_cap_param_for("github", "Issue", "issue_create", "repository");
    let p_title = map.ident_sym_cap_param_for("github", "Issue", "issue_create", "title");
    let p_body = map.ident_sym_cap_param_for("github", "Issue", "issue_create", "body");
    let p_labels = map.ident_sym_cap_param_for("github", "Issue", "issue_create", "labels");
    let repo_e = map.entity_sym_for("github", "Repository");
    let repo_owner = map.ident_sym_entity_field("Repository", "owner");
    let repo_name = map.ident_sym_entity_field("Repository", "repo");
    let source = format!(
        r#"repo = {repo_e}({repo_owner}="ryan-s-roberts", {repo_name}="tool-test")
created = {issue_e}.{method_sym}({p_repo}=repo.full_name, {p_title}="Document labels", {p_body}="guide body", {p_labels}=["bug", "docs"])
created"#,
        repo_e = repo_e,
        repo_owner = repo_owner,
        repo_name = repo_name,
    );
    let plan = compile_github_program(&session, "github-issue-create-p-syms", &source);
    let created = plan["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .find(|n| n["id"] == "created")
        .expect("created node");
    let ir_blob = created
        .get("ir")
        .filter(|v| !v.is_null())
        .or_else(|| created.get("ir_template"))
        .expect("plan IR or ir_template");
    let expr_json = ir_blob
        .pointer("/expr")
        .or_else(|| ir_blob.get("expr"))
        .expect("template expr");
    let expr: plasm_core::Expr =
        serde_json::from_value(expr_json.clone()).expect("deserialize create IR");
    let plasm_core::Expr::Create(create) = expr else {
        panic!("expected Create IR, got {expr:?}");
    };
    let plasm_core::Value::Object(input) = create.input.to_value() else {
        panic!("expected object invoke input, got {:?}", create.input);
    };
    for key in ["title", "body", "labels"] {
        assert!(
            input.contains_key(key),
            "invoke input must use logical param `{key}`, got keys {:?}",
            input.keys().collect::<Vec<_>>()
        );
    }
    assert!(
        !input
            .keys()
            .any(|k| plasm_core::symbol_tuning::SymbolMap::is_opaque_p_sym(k)),
        "invoke input must not retain opaque p# keys: {input:?}"
    );
    evaluate_plasm_plan_dry(&session, &plan).expect("staged issue_create dry-run preflight");
}

#[test]
fn pr_create_dry_run_resolves_cap_qualified_param_symbols() {
    let cgs = github_cgs();
    let session = github_ranked_mutator_session(
        &cgs,
        &["Repository", "PullRequest"],
        "open a pull request from feature branch into main",
        &["pr_create"],
        "pr_create",
    );
    let map = github_symbol_map(&session);
    let pr_e = map.entity_sym_for("github", "PullRequest");
    let cap = cgs.get_capability("pr_create").expect("pr_create");
    let method_sym = map.method_sym("PullRequest", cap.name.as_str());
    let p_repo = map.ident_sym_cap_param_for("github", "PullRequest", "pr_create", "repository");
    let p_title = map.ident_sym_cap_param_for("github", "PullRequest", "pr_create", "title");
    let p_head = map.ident_sym_cap_param_for("github", "PullRequest", "pr_create", "head");
    let p_base = map.ident_sym_cap_param_for("github", "PullRequest", "pr_create", "base");
    let repo_e = map.entity_sym_for("github", "Repository");
    let repo_owner = map.ident_sym_entity_field("Repository", "owner");
    let repo_name = map.ident_sym_entity_field("Repository", "repo");
    let source = format!(
        r#"repo = {repo_e}({repo_owner}="ryan-s-roberts", {repo_name}="tool-test")
opened = {pr_e}.{method_sym}({p_repo}=repo.full_name, {p_title}="Label guide", {p_head}="feat/label-color-guide", {p_base}="main")
opened"#,
        repo_e = repo_e,
        repo_owner = repo_owner,
        repo_name = repo_name,
        pr_e = pr_e,
        method_sym = method_sym,
        p_repo = p_repo,
        p_title = p_title,
        p_head = p_head,
        p_base = p_base,
    );
    let plan = compile_github_program(&session, "github-pr-create", &source);
    let opened = plan["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .find(|n| n["id"] == "opened")
        .expect("opened node");
    assert_eq!(opened["kind"], "create");
    evaluate_plasm_plan_dry(&session, &plan).expect("pr_create dry-run");
}

#[test]
fn repo_content_put_dry_run_resolves_cap_qualified_param_symbols() {
    let cgs = github_cgs();
    let session = github_ranked_mutator_session(
        &cgs,
        &["Repository"],
        "update repository file content on a branch",
        &["repo_content_put"],
        "repo_content_put",
    );
    let map = github_symbol_map(&session);
    let repo_e = map.entity_sym_for("github", "Repository");
    let cap = cgs
        .get_capability("repo_content_put")
        .expect("repo_content_put");
    let method_sym = map.method_sym("Repository", cap.name.as_str());
    let p_repo =
        map.ident_sym_cap_param_for("github", "Repository", "repo_content_put", "repository");
    let p_path = map.ident_sym_cap_param_for("github", "Repository", "repo_content_put", "path");
    let p_branch =
        map.ident_sym_cap_param_for("github", "Repository", "repo_content_put", "branch");
    let p_content =
        map.ident_sym_cap_param_for("github", "Repository", "repo_content_put", "content");
    let p_message =
        map.ident_sym_cap_param_for("github", "Repository", "repo_content_put", "message");
    let repo_owner = map.ident_sym_entity_field("Repository", "owner");
    let repo_name = map.ident_sym_entity_field("Repository", "repo");
    let source = format!(
        r#"written = {repo_e}({repo_owner}="ryan-s-roberts", {repo_name}="tool-test").{method_sym}({p_repo}={repo_e}({repo_owner}="ryan-s-roberts", {repo_name}="tool-test"), {p_path}="docs/LABEL_COLORS.md", {p_branch}="feat/label-color-guide", {p_content}="ZHVtbXk=", {p_message}="Add label color guide")
written"#,
        repo_e = repo_e,
        repo_owner = repo_owner,
        repo_name = repo_name,
        method_sym = method_sym,
        p_repo = p_repo,
        p_path = p_path,
        p_branch = p_branch,
        p_content = p_content,
        p_message = p_message,
    );
    let plan = compile_github_program(&session, "github-repo-content-put", &source);
    let written = plan["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .find(|n| n["id"] == "written")
        .expect("written node");
    assert_eq!(written["kind"], "action");
    evaluate_plasm_plan_dry(&session, &plan).expect("repo_content_put dry-run");
}

/// Session `m#` must resolve to the catalog capability from `sym_to_method`, never via kebab scan.
#[test]
fn issue_create_opaque_m_resolves_from_session_map() {
    let cgs = github_cgs();
    let session = github_ranked_mutator_session(
        &cgs,
        &["Repository", "Issue", "WorkflowRun"],
        "create issue and rerun failed workflow jobs",
        &["issue_create", "workflow_run_rerun_failed_jobs"],
        "issue_create",
    );
    let map = github_symbol_map(&session);
    let issue_e = map.entity_sym_for("github", "Issue");
    let issue_create_m = map.method_sym_for("github", "Issue", "issue_create");
    assert!(
        issue_create_m.starts_with('m'),
        "issue_create must have session m#: {issue_create_m}"
    );
    let p_repo = map.ident_sym_cap_param_for("github", "Issue", "issue_create", "repository");
    let p_title = map.ident_sym_cap_param_for("github", "Issue", "issue_create", "title");
    let repo_e = map.entity_sym_for("github", "Repository");
    let repo_owner = map.ident_sym_entity_field("Repository", "owner");
    let repo_name = map.ident_sym_entity_field("Repository", "repo");
    let source = format!(
        r#"repo = {repo_e}({repo_owner}="ryan-s-roberts", {repo_name}="tool-test")
created = {issue_e}.{issue_create_m}({p_repo}=repo.full_name, {p_title}="Label guide")
created"#,
        repo_e = repo_e,
        repo_owner = repo_owner,
        repo_name = repo_name,
        issue_e = issue_e,
        issue_create_m = issue_create_m,
        p_repo = p_repo,
        p_title = p_title,
    );
    let plan = compile_github_program(&session, "github-issue-create-session-m", &source);
    let created = plan["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .find(|n| n["id"] == "created")
        .expect("created node");
    assert_eq!(created["kind"], "create");
    let ir_blob = created
        .get("ir")
        .filter(|v| !v.is_null())
        .or_else(|| created.get("ir_template"))
        .expect("plan IR or ir_template");
    assert!(
        ir_blob.to_string().contains("issue_create"),
        "session m# must bind issue_create, not workflow rerun: {ir_blob}"
    );
    assert!(
        !ir_blob.to_string().contains("workflow_run_rerun"),
        "must not resolve kebab homograph: {ir_blob}"
    );
    evaluate_plasm_plan_dry(&session, &plan).expect("issue_create session m# dry-run");
}

/// Federated six-seed GitHub exposure: compile gh-77-shaped program from TSV-resolved tokens only.
#[test]
fn github_six_seed_tsv_verbatim_program_compiles() {
    let cgs = github_cgs();
    let session = github_ranked_mutator_session(
        &cgs,
        &[
            "Repository",
            "Issue",
            "Label",
            "IssueComment",
            "PullRequest",
            "PullRequestReview",
        ],
        "document repository labels, create issue, apply labels, comment",
        &["issue_create", "issue_update", "issue_comment_create"],
        "issue_create",
    );
    let map = github_symbol_map(&session);
    let repo_e = map.entity_sym_for("github", "Repository");
    let issue_e = map.entity_sym_for("github", "Issue");
    let label_e = map.entity_sym_for("github", "Label");
    let comment_e = map.entity_sym_for("github", "IssueComment");
    let repo_owner = map.ident_sym_entity_field("Repository", "owner");
    let repo_name = map.ident_sym_entity_field("Repository", "repo");
    let repo_full = map.ident_sym_entity_field("Repository", "full_name");
    let issue_number = map.ident_sym_entity_field("Issue", "number");
    let p_label_repo = map.ident_sym_cap_param_for("github", "Label", "label_query", "repository");
    let p_label_name = map.ident_sym_entity_field("Label", "name");
    let issue_create_m = map.method_sym_for("github", "Issue", "issue_create");
    let issue_update_m = map.method_sym_for("github", "Issue", "issue_update");
    let issue_comment_m = map.method_sym_for("github", "IssueComment", "issue_comment_create");
    let p_issue_create_repo =
        map.ident_sym_cap_param_for("github", "Issue", "issue_create", "repository");
    let p_issue_create_title = map.ident_sym_cap_param_for("github", "Issue", "issue_create", "title");
    let p_issue_create_body = map.ident_sym_cap_param_for("github", "Issue", "issue_create", "body");
    let p_issue_update_labels =
        map.ident_sym_cap_param_for("github", "Issue", "issue_update", "labels");
    let p_comment_repo =
        map.ident_sym_cap_param_for("github", "IssueComment", "issue_comment_create", "repository");
    let p_comment_issue =
        map.ident_sym_cap_param_for("github", "IssueComment", "issue_comment_create", "issue_number");
    let p_comment_body =
        map.ident_sym_cap_param_for("github", "IssueComment", "issue_comment_create", "body");
    let p_issue_title = map.ident_sym_entity_field("Issue", "title");
    let source = format!(
        r#"repo = {repo_e}({repo_owner}="ryan-s-roberts", {repo_name}="tool-test")
labels = {label_e}{{{p_label_repo}=repo.{repo_full}}}[{p_label_name}]
created = {issue_e}.{issue_create_m}({p_issue_create_repo}=repo.{repo_full}, {p_issue_create_title}="Label guide", {p_issue_create_body}="Demonstration issue")
updated = {issue_e}({repo_owner}="ryan-s-roberts", {repo_name}="tool-test", {issue_number}=created.{issue_number}).{issue_update_m}({p_issue_update_labels}=labels.{p_label_name})
comment = {comment_e}.{issue_comment_m}({p_comment_repo}=repo.{repo_full}, {p_comment_issue}=created.{issue_number}, {p_comment_body}="Applied all labels")
labels, created[{issue_number}, {p_issue_title}]"#,
        repo_e = repo_e,
        repo_owner = repo_owner,
        repo_name = repo_name,
        repo_full = repo_full,
        label_e = label_e,
        p_label_repo = p_label_repo,
        p_label_name = p_label_name,
        issue_e = issue_e,
        issue_create_m = issue_create_m,
        p_issue_create_repo = p_issue_create_repo,
        p_issue_create_title = p_issue_create_title,
        p_issue_create_body = p_issue_create_body,
        issue_number = issue_number,
        issue_update_m = issue_update_m,
        p_issue_update_labels = p_issue_update_labels,
        comment_e = comment_e,
        issue_comment_m = issue_comment_m,
        p_comment_repo = p_comment_repo,
        p_comment_issue = p_comment_issue,
        p_comment_body = p_comment_body,
        p_issue_title = p_issue_title,
    );
    let plan = compile_github_program(&session, "github-gh77-tsv-verbatim", &source);
    assert!(
        plan["nodes"].as_array().is_some_and(|n| !n.is_empty()),
        "gh-77-shaped TSV verbatim program must compile: {plan:?}"
    );
    evaluate_plasm_plan_dry(&session, &plan).expect("gh-77 TSV verbatim dry-run");
}
