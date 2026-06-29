//! GitHub symbol-resolution regressions (projection + cap-qualified mutator args).

use super::test_support::{
    compile_github_program, github_cgs, github_issue_label_session, github_ranked_mutator_session,
    github_symbol_map,
};
use crate::plasm_plan_run::evaluate_plasm_plan_dry;

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
    let repo_owner = map.ident_sym_entity_field("Repository", "owner");
    let repo_name = map.ident_sym_entity_field("Repository", "repo");
    let p_repository = map.ident_sym_cap_param_for("github", "Label", "label_query", "repository");
    let source = format!(
        r#"repo = Repository({repo_owner}="ryan-s-roberts", {repo_name}="tool-test")
labels = {label_e}{{{p_repository}=repo.full_name}}
labels[{p_name},{p_color},{p_desc}]"#,
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
    let method = plasm_core::capability_method_label_kebab(cap);
    let method_sym = map.method_sym("Issue", &method);
    let p_repo = map.ident_sym_cap_param_for("github", "Issue", "issue_create", "repository");
    let p_title = map.ident_sym_cap_param_for("github", "Issue", "issue_create", "title");
    let p_body = map.ident_sym_cap_param_for("github", "Issue", "issue_create", "body");
    let repo_owner = map.ident_sym_entity_field("Repository", "owner");
    let repo_name = map.ident_sym_entity_field("Repository", "repo");
    let source = format!(
        r#"repo = Repository({repo_owner}="ryan-s-roberts", {repo_name}="tool-test")
created = {issue_e}.{method_sym}({p_repo}=repo.full_name, {p_title}="Document labels", {p_body}="guide body")
created"#,
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
    let method = plasm_core::capability_method_label_kebab(cap);
    let method_sym = map.method_sym("PullRequest", &method);
    let p_repo = map.ident_sym_cap_param_for("github", "PullRequest", "pr_create", "repository");
    let p_title = map.ident_sym_cap_param_for("github", "PullRequest", "pr_create", "title");
    let p_head = map.ident_sym_cap_param_for("github", "PullRequest", "pr_create", "head");
    let p_base = map.ident_sym_cap_param_for("github", "PullRequest", "pr_create", "base");
    let repo_owner = map.ident_sym_entity_field("Repository", "owner");
    let repo_name = map.ident_sym_entity_field("Repository", "repo");
    let source = format!(
        r#"repo = Repository({repo_owner}="ryan-s-roberts", {repo_name}="tool-test")
opened = {pr_e}.{method_sym}({p_repo}=repo.full_name, {p_title}="Label guide", {p_head}="feat/label-color-guide", {p_base}="main")
opened"#,
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
    let method = plasm_core::capability_method_label_kebab(cap);
    let method_sym = map.method_sym("Repository", &method);
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
