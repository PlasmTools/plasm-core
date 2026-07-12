//! Shared intent/requirement signal detection for discovery retrieval and seed selection.

/// Workflow mutation verbs (transition, move, close, share, create, …).
pub fn intent_suggests_workflow_mutation(intent: &str) -> bool {
    let lower = intent.to_lowercase();
    WORKFLOW_MUTATION_SIGNALS
        .iter()
        .any(|signal| lower.contains(signal))
}

pub fn requirement_implies_mutation(text: &str) -> bool {
    let lower = text.to_lowercase();
    STATE_CHANGE_SIGNALS
        .iter()
        .any(|signal| lower.contains(signal))
}

pub fn requirement_implies_create_on_related(text: &str) -> bool {
    let lower = text.to_lowercase();
    CREATE_SIGNALS.iter().any(|signal| lower.contains(signal))
}

pub fn requirement_implies_issue_comment_create(text: &str) -> bool {
    let lower = text.to_lowercase();
    requirement_implies_create_on_related(text)
        && (lower.contains("comment") || lower.contains("note") || lower.contains("reply"))
}

pub fn requirement_implies_share_or_access(text: &str) -> bool {
    let lower = text.to_lowercase();
    SHARE_ACCESS_SIGNALS
        .iter()
        .any(|signal| lower.contains(signal))
}

pub fn intent_requires_non_catalog_host_capability(intent: &str) -> bool {
    let lower = intent.to_lowercase();
    HOST_CAPABILITY_SIGNALS
        .iter()
        .any(|signal| lower.contains(signal))
}

pub fn intent_names_catalog(entry_id: &str, intent: &str) -> bool {
    let lower = intent.to_lowercase();
    let catalog = entry_id.to_lowercase();
    lower.contains(&catalog) || lower.contains(&catalog.replace('-', " "))
}

/// `owner/repo`-style path token in intent (not a URL).
pub fn intent_mentions_repo_path(intent: &str) -> bool {
    intent.split_whitespace().any(|token| {
        let parts: Vec<&str> = token.split('/').collect();
        parts.len() == 2
            && !parts[0].is_empty()
            && !parts[1].is_empty()
            && !token.starts_with("http")
    })
}

pub fn intent_suggests_github_repo_workflow(intent: &str) -> bool {
    if !intent_names_catalog("github", intent) {
        return false;
    }
    let lower = intent.to_lowercase();
    intent_mentions_repo_path(intent)
        || lower.contains("repo")
        || lower.contains("branch")
        || lower.contains("pull request")
        || lower.contains(" pr")
        || lower.contains("issue")
}

pub fn catalog_mentioned_in_requirement(catalog: &str, requirement_text: &str) -> bool {
    let lower = requirement_text.to_lowercase();
    let catalog_lower = catalog.to_lowercase();
    if lower.contains(&catalog_lower) {
        return true;
    }
    match catalog_lower.as_str() {
        "google-calendar" => lower.contains("google calendar") || lower.contains("gcal"),
        "google-sheets" => lower.contains("google sheets") || lower.contains("spreadsheet"),
        "google-docs" => lower.contains("google docs"),
        "google-drive" => lower.contains("google drive"),
        "microsoft-teams" => lower.contains("microsoft teams") || lower.contains(" teams "),
        "hackernews" => lower.contains("hacker news") || lower.contains("hackernews"),
        _ => false,
    }
}

pub fn is_auxiliary_entity_for_mutation(entity: &str) -> bool {
    entity.contains("Snapshot") || entity.ends_with("Context") || entity == "Board"
}

/// Primary workflow roots eligible for mutation inject and auxiliary replacement.
pub const WORKFLOW_MUTATION_ENTITIES: &[&str] = &[
    "Issue",
    "Transition",
    "Dashboard",
    "PullRequest",
    "MergeRequest",
    "Page",
    "Document",
    "IssueComment",
    "Comment",
    "Message",
];

const WORKFLOW_MUTATION_SIGNALS: &[&str] = &[
    "transition",
    "move ",
    "move the",
    "to done",
    "close ",
    "update ",
    "append ",
    "pin ",
    "grant ",
    "share ",
    "publish ",
    "sign-off",
    "sign off",
    "change status",
    "add a",
    "post a",
    "create ",
];

const STATE_CHANGE_SIGNALS: &[&str] = &[
    "transition",
    "move ",
    "move the",
    "close ",
    "to done",
    "sign-off",
    "sign off",
    "change status",
    "update status",
];

const CREATE_SIGNALS: &[&str] = &[
    "add ",
    "create ",
    "post a",
    "post ",
    "write a",
    "leave a",
    "triage note",
];

const SHARE_ACCESS_SIGNALS: &[&str] = &[
    "share",
    "grant",
    "publish",
    "access",
    "view-only",
    "read-only",
    "permission",
];

const HOST_CAPABILITY_SIGNALS: &[&str] = &[
    "fine-tune",
    "finetune",
    "fine tune",
    "train model",
    "train a model",
    "ml model",
    "machine learning",
    "classifier training",
    "model training",
    "stripe",
    "payment processing",
    "refund payment",
    "process refund",
    "terraform",
    "kubernetes",
    "k8s deploy",
    "infra deploy",
    "zoom",
    "webinar hosting",
    "host webinar",
];
