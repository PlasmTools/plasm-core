//! Static + runtime CGS↔CML path / GraphQL identity-env vocabulary.
//!
//! Pack proof and projection share one ledger: every identity-env var is either
//! identity-projectable under a named [`SoleAliasPolicy`] or a declared capability input.

mod names;
mod project;
mod prove;

pub use names::{
    capability_template_all_var_names, classify_path_var, graphql_operation_variable_names,
    identity_env_var_names, identity_wire_names, is_identity_projectable,
    path_var_names_from_mapping_json, IdentityEnvVars, IdentityNameMatch, IdentityWireNames,
    SoleAliasPolicy,
};
pub use project::{
    capability_declared_input_names, create_binds_from_anchor_identity,
    project_capability_identity_env, project_identity_onto_vars, CapabilityIdentityProjection,
    CmlIdentityEnv, PathEnvProjectionError,
};
pub use prove::{prove_path_env_coverage, prove_path_env_coverage_in_cgs, PathEnvProofError};
