//! Compiler-issued static catalogue dependencies of a lowered, typechecked computation.
//! This evidence is not proof of full Python admission or successful execution.
use crate::execute_session::ExecuteSession;
use plasm_core::{prerequisites::CapabilityRef, Expr, PlasmComp, PlasmStepPayload};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, thiserror::Error)]
enum DependencyError {
    #[error("catalogue unavailable: {0}")]
    CatalogDrift(String),
    #[error("query capability resolution: {0}")]
    QueryDependency(#[source] Box<plasm_core::query_resolve::QueryCapabilityResolveError>),
    #[error("get capability resolution: {0}")]
    GetDependency(#[source] Box<plasm_core::TypeError>),
    #[error("external session execution state")]
    ExternalSessionDependency,
    #[error("missing admitted compiler dependency")]
    MissingCompilerDependency,
    #[error("relation unavailable: {catalog}::{relation}")]
    RelationDependencyUnavailable { catalog: String, relation: String },
    #[error(transparent)]
    Relation(#[from] plasm_core::relation_materialize::dependencies::DependencyError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum DependencyUnavailable {
    #[error("dependency evidence was not captured")]
    NotCaptured,
    #[error("dependency evidence belongs to a different computation")]
    ComputationChanged,
    #[error("program depends on external session state")]
    ExternalSession,
    #[error("admitted compiler dependency is missing")]
    MissingCompilerEvidence,
    #[error("catalogue dependency resolution failed: {diagnostic}")]
    CatalogueResolution { diagnostic: String },
    #[error("compiled dependency digest encoding failed: {diagnostic}")]
    Encoding { diagnostic: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompiledCatalogueDependencies {
    compiled_sha256: String,
    capabilities: BTreeSet<CapabilityRef>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
pub enum DependencyEvidence {
    Available(CompiledCatalogueDependencies),
    Unavailable(DependencyUnavailable),
}
impl Default for DependencyEvidence {
    fn default() -> Self {
        Self::Unavailable(DependencyUnavailable::NotCaptured)
    }
}
impl DependencyEvidence {
    pub(crate) fn capture(session: &ExecuteSession, comp: &PlasmComp) -> Self {
        let mut capabilities = BTreeSet::new();
        if let Err(error) = collect(session, comp, &mut capabilities) {
            return Self::Unavailable(match error {
                DependencyError::ExternalSessionDependency => {
                    DependencyUnavailable::ExternalSession
                }
                DependencyError::MissingCompilerDependency => {
                    DependencyUnavailable::MissingCompilerEvidence
                }
                error => DependencyUnavailable::CatalogueResolution {
                    diagnostic: error.to_string(),
                },
            });
        }
        match digest(comp) {
            Ok(compiled_sha256) => Self::Available(CompiledCatalogueDependencies {
                compiled_sha256,
                capabilities,
            }),
            Err(error) => Self::Unavailable(error),
        }
    }
    pub fn capabilities(
        &self,
        comp: &PlasmComp,
    ) -> Result<&BTreeSet<CapabilityRef>, DependencyUnavailable> {
        match self {
            Self::Available(evidence) if evidence.compiled_sha256 == digest(comp)? => {
                Ok(&evidence.capabilities)
            }
            Self::Available(_) => Err(DependencyUnavailable::ComputationChanged),
            Self::Unavailable(reason) => Err(reason.clone()),
        }
    }
}
fn digest(comp: &PlasmComp) -> Result<String, DependencyUnavailable> {
    serde_json::to_value(comp)
        .and_then(|value| serde_json::to_vec(&value))
        .map(|bytes| plasm_core::catalog_discovery::content_hash(&bytes))
        .map_err(|error| DependencyUnavailable::Encoding {
            diagnostic: error.to_string(),
        })
}
fn catalog<'a>(
    session: &'a ExecuteSession,
    entry: &str,
) -> Result<&'a plasm_core::CGS, DependencyError> {
    session
        .contexts_by_entry
        .get(entry)
        .map(|context| context.cgs.as_ref())
        .or_else(|| (session.entry_id == entry).then_some(session.cgs.as_ref()))
        .ok_or_else(|| DependencyError::CatalogDrift(entry.into()))
}

fn expression(
    session: &ExecuteSession,
    entry: &str,
    expr: &Expr,
    caps: &mut BTreeSet<CapabilityRef>,
) -> Result<(), DependencyError> {
    let cgs = catalog(session, entry)?;
    let capability = match expr {
        Expr::Query(query) => Some(
            plasm_core::query_resolve::resolve_query_capability(query, cgs)
                .map_err(|error| DependencyError::QueryDependency(Box::new(error)))?,
        ),
        Expr::Get(get) => Some(
            get.capability(cgs)
                .map_err(|error| DependencyError::GetDependency(Box::new(error)))?,
        ),
        Expr::Invoke(invoke) => cgs.get_capability(invoke.capability.as_str()),
        Expr::Create(create) => cgs.get_capability(create.capability.as_str()),
        Expr::Delete(delete) => cgs.get_capability(delete.capability.as_str()),
        // Relation traversal requires materialization/prerequisite closure proof
        // beyond a source-token ledger. Never fabricate a guessed capability.
        Expr::Chain(chain) => {
            expression(session, entry, &chain.source, caps)?;
            let parent = chain
                .source
                .relation_navigation_entity(cgs)
                .ok_or(DependencyError::MissingCompilerDependency)?;
            let relation = cgs
                .get_entity(&parent)
                .and_then(|entity| entity.relations.get(chain.selector.as_str()))
                .ok_or_else(|| DependencyError::RelationDependencyUnavailable {
                    catalog: entry.into(),
                    relation: chain.selector.clone(),
                })?;
            return relation_requirements(
                session,
                RelationDependency {
                    source_entry: entry,
                    parent: &parent,
                    target_entry: entry,
                    target: relation.target_resource.as_str(),
                    materialize: relation.materialize.as_ref(),
                },
                caps,
            );
        }
        Expr::Page(_) | Expr::Wait(_) | Expr::Cancel(_) => {
            return Err(DependencyError::ExternalSessionDependency);
        }
        Expr::TeachingValue { .. } => return Err(DependencyError::MissingCompilerDependency),
    }
    .ok_or_else(|| DependencyError::CatalogDrift(entry.into()))?;
    caps.insert(CapabilityRef {
        catalog: entry.into(),
        capability: capability.name.to_string(),
    });
    Ok(())
}

struct RelationDependency<'a> {
    source_entry: &'a str,
    parent: &'a str,
    target_entry: &'a str,
    target: &'a str,
    materialize: Option<&'a plasm_core::RelationMaterialization>,
}
fn relation_requirements(
    session: &ExecuteSession,
    dependency: RelationDependency<'_>,
    caps: &mut BTreeSet<CapabilityRef>,
) -> Result<(), DependencyError> {
    let dependencies = plasm_core::relation_materialize::dependencies::resolve(
        catalog(session, dependency.source_entry)?,
        catalog(session, dependency.target_entry)?,
        dependency.parent,
        dependency.target,
        dependency.materialize,
    )
    .map_err(DependencyError::Relation)?;
    for dependency_capability in dependencies {
        let entry = match dependency_capability.owner {
            plasm_core::relation_materialize::dependencies::Owner::Source => {
                dependency.source_entry
            }
            plasm_core::relation_materialize::dependencies::Owner::Target => {
                dependency.target_entry
            }
        };
        caps.insert(CapabilityRef {
            catalog: entry.into(),
            capability: dependency_capability.capability,
        });
    }
    Ok(())
}

fn collect(
    session: &ExecuteSession,
    comp: &PlasmComp,
    caps: &mut BTreeSet<CapabilityRef>,
) -> Result<(), DependencyError> {
    for step in comp.steps.values() {
        match step {
            PlasmStepPayload::Invoke(invoke) => {
                let owner = invoke
                    .qualified_entity
                    .as_ref()
                    .ok_or(DependencyError::MissingCompilerDependency)?;
                let expr = invoke
                    .ir
                    .as_ref()
                    .map(|ir| &ir.expr)
                    .or_else(|| invoke.ir_template.as_ref().map(|ir| &ir.expr))
                    .ok_or(DependencyError::MissingCompilerDependency)?;
                expression(session, owner.entry_id.as_str(), expr, caps)?;
            }
            PlasmStepPayload::FlatMapApply(apply) => expression(
                session,
                apply.effect_template.qualified_entity.entry_id.as_str(),
                &apply.effect_template.ir_template.expr,
                caps,
            )?,
            PlasmStepPayload::UnfoldUntil(unfold) => {
                expression(
                    session,
                    unfold.effect_template.qualified_entity.entry_id.as_str(),
                    &unfold.effect_template.ir_template.expr,
                    caps,
                )?;
                if let Some(seed) = &unfold.seed_ir {
                    expression(
                        session,
                        unfold.effect_template.qualified_entity.entry_id.as_str(),
                        &seed.expr,
                        caps,
                    )?;
                }
                if let Some(body) = &unfold.step_scope {
                    collect(session, &body.body, caps)?;
                }
                if let Some(body) = &unfold.until_scope {
                    collect(session, &body.body, caps)?;
                }
            }
            PlasmStepPayload::MapBody(body) => collect(session, &body.body, caps)?,
            PlasmStepPayload::FlatMapRelation(payload) => {
                let relation = &payload.relation;
                let Expr::Chain(chain) = &relation.ir.expr else {
                    return Err(DependencyError::MissingCompilerDependency);
                };
                let source_entry = chain
                    .source
                    .session_catalog_entry_id()
                    .ok_or(DependencyError::MissingCompilerDependency)?
                    .as_str();
                let cgs = catalog(session, source_entry)?;
                let parent = chain
                    .source
                    .relation_navigation_entity(cgs)
                    .ok_or(DependencyError::MissingCompilerDependency)?;
                relation_requirements(
                    session,
                    RelationDependency {
                        source_entry,
                        parent: &parent,
                        target_entry: &relation.target.entry_id,
                        target: &relation.target.entity,
                        materialize: relation.materialize.as_ref(),
                    },
                    caps,
                )?;
            }
            PlasmStepPayload::Pure(value) if value.recorded_output.is_some() => {
                return Err(DependencyError::ExternalSessionDependency);
            }
            PlasmStepPayload::Pure(_) | PlasmStepPayload::Map(_) | PlasmStepPayload::Derive(_) => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod dependency_tests {
    use super::*;
    fn session(entities: &[&str]) -> ExecuteSession {
        let root = std::path::PathBuf::from(crate::test_manifest_dir());
        let cgs = std::sync::Arc::new(
            plasm_core::loader::load_schema_dir(
                &root.join("../../fixtures/schemas/plasm_language_matrix"),
            )
            .unwrap(),
        );
        let mut session = crate::test_support::session_fixtures::ExecuteSessionFixture::new()
            .entry_id("langmatrix")
            .entities(entities.iter().map(|s| s.to_string()).collect())
            .build(cgs.clone());
        session.teaching_exposure = Some(plasm_core::TeachingExposureSession::new(
            &cgs,
            "langmatrix",
            entities,
        ));
        session
    }

    #[test]
    fn compiler_evidence_is_bound_to_complete_computation_and_survives_serialization() {
        let session = session(&["LangItem"]);
        let source =
            "class Example(Program):\n    def build(self):\n        return e1.query().take(1)\n";
        let bundle = crate::plasm_dag::compile_python_program_checked(&session, source).unwrap();
        assert!(!bundle.compiled_dependencies().unwrap().is_empty());
        let encoded = serde_json::to_vec(bundle.compilation_symbols()).unwrap();
        let restored = serde_json::from_slice(&encoded).unwrap();
        let replay = bundle.clone().with_compilation_symbols(restored);
        assert_eq!(
            bundle.compiled_dependencies().unwrap(),
            replay.compiled_dependencies().unwrap()
        );
        let evidence = DependencyEvidence::capture(&session, &bundle.artifact().comp);
        let mut changed = bundle.artifact().comp.clone();
        changed.steps.clear();
        assert!(matches!(
            evidence.capabilities(&changed),
            Err(DependencyUnavailable::ComputationChanged)
        ));
        assert!(matches!(
            DependencyEvidence::default().capabilities(&bundle.artifact().comp),
            Err(DependencyUnavailable::NotCaptured)
        ));
    }

    #[test]
    fn example_dependencies_resolve_scoped_queries_through_production_selection() {
        let session = session(&["LangLine"]);
        assert!(session.cgs.primary_query_capability("LangLine").is_none());
        let query = plasm_core::QueryExpr::filtered(
            "LangLine",
            plasm_core::Predicate::eq("item_id", "fixture-item"),
        );
        let mut caps = BTreeSet::new();
        expression(&session, "langmatrix", &Expr::Query(query), &mut caps).unwrap();
        assert_eq!(
            caps,
            BTreeSet::from([CapabilityRef {
                catalog: "langmatrix".into(),
                capability: "langline_query".into(),
            }])
        );
        assert!(matches!(
            expression(
                &session,
                "langmatrix",
                &Expr::Query(plasm_core::QueryExpr::all("LangLine")),
                &mut BTreeSet::new(),
            ),
            Err(DependencyError::QueryDependency(_))
        ));
    }

    #[test]
    fn example_dependencies_reject_wrong_entity_get_contract() {
        let session = session(&["LangLine", "LangItem"]);
        let mut get =
            plasm_core::GetExpr::from_ref(plasm_core::Ref::new("LangItem", "fixture-item"));
        get.capability_name = Some("langline_get".into());
        assert!(matches!(
            expression(
                &session,
                "langmatrix",
                &Expr::Get(get),
                &mut BTreeSet::new()
            ),
            Err(DependencyError::GetDependency(_))
        ));
    }
}
