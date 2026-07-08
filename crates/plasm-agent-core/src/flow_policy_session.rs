//! Resolve published project flow policy for execute session pinning.

use crate::flow_policy_env::flow_policy_from_env_or_default;
use crate::plan_flow_policy::FlowPolicySnapshot;
use crate::server_state::PlasmHostState;

pub async fn resolve_project_flow_policy(
    st: &PlasmHostState,
    tenant_id: &str,
    workspace_slug: &str,
    project_slug: &str,
) -> FlowPolicySnapshot {
    let Some(repo) = st.flow_policy_repository() else {
        return flow_policy_from_env_or_default();
    };
    match repo
        .get_or_default(tenant_id, workspace_slug, project_slug)
        .await
    {
        Ok(row) => row.published_snapshot(),
        Err(e) => {
            tracing::warn!(
                tenant_id = %tenant_id,
                workspace_slug = %workspace_slug,
                project_slug = %project_slug,
                error = %e,
                "flow policy load failed — using inactive default"
            );
            FlowPolicySnapshot::inactive_default()
        }
    }
}

pub async fn resolve_flow_policy_for_principal(
    st: &PlasmHostState,
    tenant_id: &str,
    subject: &str,
) -> FlowPolicySnapshot {
    let Some(binding) = st.tenant_binding() else {
        return flow_policy_from_env_or_default();
    };
    match binding.get_by_subject(subject).await {
        Ok(Some(row)) => {
            resolve_project_flow_policy(
                st,
                tenant_id,
                row.workspace_slug.as_str(),
                row.project_slug.as_str(),
            )
            .await
        }
        _ => FlowPolicySnapshot::inactive_default(),
    }
}
