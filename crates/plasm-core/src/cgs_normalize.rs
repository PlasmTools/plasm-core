//! Post-assemble CGS normalization: synthesize executable relation materialization.

use indexmap::IndexMap;
use tracing::warn;

use crate::schema::{
    CapabilityKind, LegacyViaParamPatch, RelationMaterialization, RelationSchema, CGS,
};
use crate::{CapabilityParamName, EntityFieldName, EntityName, RelationName};

impl CGS {
    /// Fill missing many-relation materialization: legacy `via_param` → `query_scoped`, then
    /// `views.relation_outputs` → `view_embed`.
    pub fn normalize_relation_materialization(&mut self, legacy_via_param: &[LegacyViaParamPatch]) {
        self.apply_legacy_via_param_materialization(legacy_via_param);
        self.synthesize_view_embed_materialization();
    }

    fn apply_legacy_via_param_materialization(&mut self, patches: &[LegacyViaParamPatch]) {
        for patch in patches {
            let mat = {
                let Some(cgs_entity) = self.entities.get(&patch.entity) else {
                    continue;
                };
                let Some(relation) = cgs_entity.relations.get(&patch.relation) else {
                    continue;
                };
                if relation.materialize.is_some() {
                    continue;
                }
                infer_query_scoped_from_via_param(
                    self,
                    relation,
                    &patch.via_param,
                    &patch.source_id_field,
                )
            };
            if let Some(mat) = mat {
                if let Some(cgs_entity) = self.entities.get_mut(&patch.entity) {
                    if let Some(relation) = cgs_entity.relations.get_mut(&patch.relation) {
                        relation.materialize = Some(mat);
                    }
                }
            }
        }
    }

    fn synthesize_view_embed_materialization(&mut self) {
        let view_backing = self.view_relation_backing_index();
        let mut patches: Vec<(EntityName, RelationName, RelationMaterialization)> = Vec::new();
        for (entity_name, entity) in &self.entities {
            for (relation_name, relation) in &entity.relations {
                if relation.cardinality != crate::schema::Cardinality::Many {
                    continue;
                }
                if relation.materialize.is_some() {
                    continue;
                }
                if let Some(view_key) =
                    view_backing.get(&(entity_name.clone(), relation_name.clone()))
                {
                    patches.push((
                        entity_name.clone(),
                        relation_name.clone(),
                        RelationMaterialization::ViewEmbed {
                            view: view_key.clone(),
                        },
                    ));
                }
            }
        }
        for (entity_name, relation_name, materialize) in patches {
            warn!(
                entity = %entity_name,
                relation = %relation_name,
                "synthesized view_embed materialization from views.relation_outputs; declare materialize explicitly in domain.yaml"
            );
            let Some(entity) = self.entities.get_mut(&entity_name) else {
                continue;
            };
            let Some(relation) = entity.relations.get_mut(&relation_name) else {
                continue;
            };
            relation.materialize = Some(materialize);
        }
    }

    fn view_relation_backing_index(&self) -> IndexMap<(EntityName, RelationName), String> {
        let mut out = IndexMap::new();
        for (view_key, view) in &self.views {
            for ro in &view.relation_outputs {
                if ro.cardinality != crate::schema::Cardinality::Many {
                    continue;
                }
                out.insert(
                    (
                        view.entity.clone(),
                        RelationName::from(ro.relation.as_str()),
                    ),
                    view_key.clone(),
                );
            }
        }
        out
    }
}

/// Infer `query_scoped` from legacy domain `via_param:` after capabilities are assembled.
pub(crate) fn infer_query_scoped_from_via_param(
    cgs: &CGS,
    relation: &RelationSchema,
    via_param: &CapabilityParamName,
    source_id_field: &EntityFieldName,
) -> Option<RelationMaterialization> {
    if via_param.as_str() != source_id_field.as_str() {
        return None;
    }
    let target = relation.target_resource.as_str();
    let mut candidates: Vec<&crate::CapabilitySchema> = cgs
        .capabilities
        .values()
        .filter(|cap| cap.domain.as_str() == target)
        .filter(|cap| matches!(cap.kind, CapabilityKind::Query | CapabilityKind::Search))
        .collect();
    candidates.sort_by_key(|cap| cap.name.as_str());
    for cap in candidates {
        let fields = cap.object_params()?;
        if fields.iter().any(|f| f.name.as_str() == via_param.as_str()) {
            return Some(RelationMaterialization::QueryScoped {
                capability: cap.name.clone(),
                param: via_param.clone(),
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use crate::schema::RelationMaterialization;
    use crate::SchemaError;
    use std::path::Path;

    #[test]
    fn normalize_synthesizes_view_embed_from_relation_outputs() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix_views");
        if !dir.is_dir() {
            return;
        }
        let mut cgs = crate::loader::load_schema_dir_unvalidated(&dir).expect("load unvalidated");
        let tags_before = cgs
            .get_entity("LangTriageContext")
            .expect("LangTriageContext")
            .relations
            .get("tags")
            .expect("tags")
            .materialize
            .clone();
        assert!(
            tags_before.is_none(),
            "fixture YAML omits materialize; synthesis is the test"
        );
        cgs.normalize_relation_materialization(&[]);
        let tags_after = cgs
            .get_entity("LangTriageContext")
            .unwrap()
            .relations
            .get("tags")
            .unwrap()
            .materialize
            .as_ref()
            .expect("normalize must synthesize view_embed");
        assert_eq!(
            tags_after,
            &RelationMaterialization::ViewEmbed {
                view: "lang_triage_context".to_string(),
            }
        );
        cgs.validate()
            .expect("matrix views validates after normalize");
    }

    #[test]
    fn validate_rejects_unexecutable_many_relation() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix_views");
        if !dir.is_dir() {
            return;
        }
        let cgs = crate::loader::load_schema_dir_unvalidated(&dir).expect("load unvalidated");
        let err = cgs
            .validate()
            .expect_err("many-relation without materialize");
        assert!(
            matches!(err, SchemaError::RelationNotExecutable { .. }),
            "expected RelationNotExecutable, got {err:?}"
        );
    }
}
