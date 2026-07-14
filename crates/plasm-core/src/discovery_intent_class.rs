//! Deterministic retrieval policy for discovery candidate construction.

use std::fmt;

/// Candidate-pool policy inferred from catalog metadata and intent structure.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum DiscoveryIntentClass {
    HostCapabilityMiss {
        summary: String,
    },
    CatalogExploration,
    ReadListNav,
    ReadListLeafCollection,
    LocalizedMutation,
    RepoScopedWorkflow,
    #[default]
    WorkflowMutation,
}

impl DiscoveryIntentClass {
    pub fn wire_name(&self) -> &'static str {
        match self {
            Self::HostCapabilityMiss { .. } => "host_capability_miss",
            Self::CatalogExploration => "catalog_exploration",
            Self::ReadListNav => "read_list_nav",
            Self::ReadListLeafCollection => "read_list_leaf_collection",
            Self::LocalizedMutation => "localized_mutation",
            Self::RepoScopedWorkflow => "repo_scoped_workflow",
            Self::WorkflowMutation => "workflow_mutation",
        }
    }

    pub fn is_read_list_nav(&self) -> bool {
        matches!(self, Self::ReadListNav | Self::ReadListLeafCollection)
    }

    pub fn is_read_list_leaf_collection(&self) -> bool {
        matches!(self, Self::ReadListLeafCollection)
    }

    pub fn is_mutation_family(&self) -> bool {
        matches!(
            self,
            Self::LocalizedMutation | Self::RepoScopedWorkflow | Self::WorkflowMutation
        )
    }

    pub fn allows_workflow_inject(&self) -> bool {
        self.is_mutation_family()
    }

    pub fn allows_mutation_inject(&self) -> bool {
        self.allows_workflow_inject()
    }
}

impl fmt::Display for DiscoveryIntentClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.wire_name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutation_family_predicates() {
        assert!(DiscoveryIntentClass::WorkflowMutation.is_mutation_family());
        assert!(!DiscoveryIntentClass::ReadListNav.is_mutation_family());
        assert!(!DiscoveryIntentClass::CatalogExploration.allows_workflow_inject());
    }
}
