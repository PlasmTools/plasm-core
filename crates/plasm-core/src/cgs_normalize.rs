//! Post-assemble CGS normalization: synthesize executable relation materialization.

use indexmap::IndexMap;

use crate::schema::{CapabilityKind, CGS, RelationMaterialization, RelationSchema};
use crate::{CapabilityParamName, EntityName, RelationName};

struct RelationMaterializePatch {
    materialize: RelationMaterialization,
    clear_legacy_via_param: bool,
}

impl CGS {
    /// Fill missing many-relation materialization from views, legacy `via_param`, and GET embed keys.
    pub fn normalize_relation_materialization(&mut self) {
        let view_backing = self.view_relation_backing_index();
        let mut patches: Vec<(EntityName, RelationName, RelationMaterializePatch)> = Vec::new();
        for (entity_name, entity) in &self.entities {
            let id_field = entity.id_field.clone();
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
                        RelationMaterializePatch {
                            materialize: RelationMaterialization::ViewEmbed {
                                view: view_key.clone(),
                                relation: relation_name.clone(),
                            },
                            clear_legacy_via_param: false,
                        },
                    ));
                    continue;
                }
                if let Some(param) = relation.legacy_via_param.clone() {
                    if let Some(mat) = infer_query_scoped(self, relation, &param, &id_field) {
                        patches.push((
                            entity_name.clone(),
                            relation_name.clone(),
                            RelationMaterializePatch {
                                materialize: mat,
                                clear_legacy_via_param: true,
                            },
                        ));
                    }
                }
            }
        }
        for (entity_name, relation_name, patch) in patches {
            let Some(entity) = self.entities.get_mut(&entity_name) else {
                continue;
            };
            let Some(relation) = entity.relations.get_mut(&relation_name) else {
                continue;
            };
            relation.materialize = Some(patch.materialize);
            if patch.clear_legacy_via_param {
                relation.legacy_via_param = None;
            }
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
                    (view.entity.clone(), RelationName::from(ro.relation.as_str())),
                    view_key.clone(),
                );
            }
        }
        out
    }
}

fn infer_query_scoped(
    cgs: &CGS,
    relation: &RelationSchema,
    via_param: &CapabilityParamName,
    source_id_field: &crate::EntityFieldName,
) -> Option<RelationMaterialization> {
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
        if !fields.iter().any(|f| f.name.as_str() == via_param.as_str()) {
            continue;
        }
        let ok = via_param.as_str() == source_id_field.as_str()
            || source_id_field.as_str() == "id"
            || (via_param.as_str().ends_with("_id") && source_id_field.as_str().ends_with("_id"));
        if !ok {
            continue;
        }
        return Some(RelationMaterialization::QueryScoped {
            capability: cap.name.clone(),
            param: via_param.clone(),
        });
    }
    None
}
