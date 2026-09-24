//! Readable entity fields, derived once from direct output and identity hydration.
use crate::{CapabilityKind, CapabilityName, CapabilitySchema, EntityFieldName, EntityName, CGS};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FieldAvailability {
    Direct,
    Hydrated { capability: CapabilityName },
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntityReadProjection {
    pub entity: EntityName,
    pub fields: BTreeMap<EntityFieldName, FieldAvailability>,
}

impl EntityReadProjection {
    pub fn available_fields(&self) -> impl Iterator<Item = &str> {
        self.fields.iter().filter_map(|(name, availability)| {
            (!matches!(availability, FieldAvailability::Unavailable)).then_some(name.as_str())
        })
    }
}

impl CGS {
    /// The same identity-preserving detail read used by query hydration. List-backed
    /// reads cannot hydrate their own list without recursively re-entering it.
    pub fn query_hydration_capability(&self, entity: &str) -> Option<&CapabilitySchema> {
        self.find_capability(entity, CapabilityKind::Get)
            .filter(|cap| {
                cap.receiver_entity().map(|receiver| receiver.as_str()) == Some(entity)
                    && !cap.is_list_backed_get(self)
            })
    }

    /// Static availability, not proof of successful IO. A permitted source supplies
    /// direct fields; a permitted identity Get may supply additional fields when its
    /// required inputs are inherited. Runtime still checks actual bindings and failures.
    pub fn entity_read_projection(
        &self,
        source: &CapabilitySchema,
        permitted: impl Fn(&CapabilitySchema) -> bool,
    ) -> EntityReadProjection {
        let mut projection = EntityReadProjection {
            entity: source.domain.clone(),
            fields: BTreeMap::new(),
        };
        let Some(entity) = self.get_entity(source.domain.as_str()) else {
            return projection;
        };
        projection.fields.extend(
            entity
                .fields
                .keys()
                .cloned()
                .map(|field| (field, FieldAvailability::Unavailable)),
        );
        if !matches!(
            source.kind,
            CapabilityKind::Get | CapabilityKind::Query | CapabilityKind::Search
        ) || !permitted(source)
        {
            return projection;
        }
        let direct = self.effective_provides(source);
        for field in &direct {
            if let Some(availability) = projection.fields.get_mut(field.as_str()) {
                *availability = FieldAvailability::Direct;
            }
        }
        if !direct.iter().any(|field| field == entity.id_field.as_str())
            || !entity.key_vars.iter().all(|key| {
                key == &entity.id_field || direct.iter().any(|field| field == key.as_str())
            })
        {
            return projection;
        }
        for (field, providers) in self.read_field_providers(source.domain.as_str()) {
            if !matches!(
                projection.fields.get(field.as_str()),
                Some(FieldAvailability::Unavailable)
            ) {
                continue;
            }
            for name in providers {
                let get = &self.capabilities[name.as_str()];
                if !permitted(get) {
                    continue;
                }
                let required_inputs_available = get
                    .input_fields()
                    .filter(|field| field.required && field.default.is_none())
                    .all(|field| {
                        field.name == entity.id_field.as_str()
                            || entity.key_vars.iter().any(|key| key.as_str() == field.name)
                            || source.input_fields().any(|parent| {
                                parent.name == field.name
                                    && parent.wire == field.wire
                                    && (parent.required || parent.default.is_some())
                            })
                    });
                if required_inputs_available {
                    projection.fields.insert(
                        field.clone().into(),
                        FieldAvailability::Hydrated {
                            capability: get.name.clone(),
                        },
                    );
                    break;
                }
            }
        }
        projection
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prerequisites::{
        project_input_source_candidates, CapabilityRef, InputSourceTarget, RowIdentity,
    };
    use proptest::prelude::*;

    fn fixture() -> CGS {
        crate::loader::load_schema_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/entity_projection_matrix"),
        )
        .unwrap()
    }

    #[test]
    fn receiver_entity_is_independent_of_operation_owner() {
        let mut cgs = fixture();
        cgs.capabilities.get_mut("apply").unwrap().domain = "Operation".into();
        let catalogs = BTreeMap::from([("matrix".into(), &cgs)]);
        let candidates = project_input_source_candidates(
            &catalogs,
            &[CapabilityRef {
                catalog: "matrix".into(),
                capability: "apply".into(),
            }],
            &|_| true,
        )
        .unwrap();
        assert!(candidates
            .iter()
            .any(|candidate| candidate.provider.capability == "list"));
        assert!(!candidates
            .iter()
            .any(|candidate| candidate.provider.capability == "operation_get"));
    }

    #[test]
    fn arbitrary_id_argument_is_not_declared_receiver_identity() {
        let mut cgs = fixture();
        let entity = cgs.entities.get_mut("Record").unwrap();
        entity.id_field = "key".into();
        let mut identity = entity.fields.shift_remove("id").unwrap();
        identity.name = "key".into();
        entity.fields.insert("key".into(), identity);
        for cap in cgs
            .capabilities
            .values_mut()
            .filter(|cap| cap.domain.as_str() == "Record")
        {
            for field in &mut cap.provides {
                if field == "id" {
                    *field = "key".into();
                }
            }
        }
        let secret = cgs.capabilities.get_mut("secret").unwrap();
        let crate::schema::InputType::Object { fields, .. } =
            &mut secret.inputs.arguments.as_mut().unwrap().input_type
        else {
            unreachable!()
        };
        fields[0].name = "id".into();
        let projection = cgs.entity_read_projection(&cgs.capabilities["list"], |_| true);
        assert_eq!(
            projection.fields["email"],
            FieldAvailability::Hydrated {
                capability: "detail".into()
            }
        );
        assert_eq!(projection.fields["secret"], FieldAvailability::Unavailable);
        let wire: CGS = serde_json::from_slice(&serde_json::to_vec(&cgs).unwrap()).unwrap();
        assert_eq!(
            projection,
            wire.entity_read_projection(&wire.capabilities["list"], |_| true)
        );
    }

    #[test]
    fn missing_identity_blocks_implicit_hydration() {
        let mut cgs = fixture();
        cgs.capabilities.get_mut("list").unwrap().provides = vec!["unavailable".into()];
        let projection = cgs.entity_read_projection(&cgs.capabilities["list"], |_| true);
        assert_eq!(projection.fields["email"], FieldAvailability::Unavailable);
        assert_eq!(projection.fields["unavailable"], FieldAvailability::Direct);
    }

    proptest! {
        #[test]
        fn projection_preserves_availability_and_authority_across_serialization(allow_detail in any::<bool>(), allow_list in any::<bool>()) {
            let cgs = fixture();
            let decoded: CGS = serde_json::from_slice(&serde_json::to_vec(&cgs).unwrap()).unwrap();
            let permitted = |cap: &CapabilitySchema| (cap.name.as_str() != "detail" || allow_detail) && (cap.name.as_str() != "list" || allow_list);
            let projection = cgs.entity_read_projection(&cgs.capabilities["list"], permitted);
            prop_assert_eq!(&projection, &decoded.entity_read_projection(&decoded.capabilities["list"], permitted));
            let wire: EntityReadProjection = serde_json::from_slice(&serde_json::to_vec(&projection).unwrap()).unwrap();
            prop_assert_eq!(&wire, &projection);
            prop_assert_eq!(projection.available_fields().any(|f| f == "id"), allow_list);
            prop_assert_eq!(projection.available_fields().any(|f| f == "email"), allow_list && allow_detail);
            prop_assert!(!projection.available_fields().any(|f| f == "secret" || f == "unavailable"));
            if allow_list && allow_detail {
                prop_assert_eq!(&projection.fields["email"], &FieldAvailability::Hydrated { capability: "detail".into() });
            }
        }

        #[test]
        fn receiver_sources_and_hydrated_membership_obey_authority(allow_detail in any::<bool>(), allow_directory in any::<bool>()) {
            let cgs = fixture();
            let catalogs = BTreeMap::from([("matrix".into(), &cgs)]);
            let permitted = |cap: &CapabilityRef| (cap.capability != "detail" || allow_detail) && (cap.capability != "directory" || allow_directory);
            let business = [CapabilityRef { catalog: "matrix".into(), capability: "apply".into() }];
            let candidates = project_input_source_candidates(&catalogs, &business, &permitted).unwrap();
            let source = candidates.iter().find(|c| c.provider.capability == "list").unwrap();
            prop_assert!(source.bindings.iter().any(|b| matches!(&b.input, InputSourceTarget::Receiver { entity } if entity.as_str() == "Record")), "typed receiver witness");
            prop_assert_eq!(candidates.iter().any(|c| c.provider.capability == "directory"), allow_detail && allow_directory);
            if allow_detail && allow_directory {
                let directory = candidates.iter().find(|c| c.provider.capability == "directory").unwrap();
                prop_assert!(directory.membership.iter().any(|m| m.source.capability == "list" && m.source_identity == RowIdentity::Field { field: "email".into() }), "hydrated membership witness");
            }
            let wire: Vec<crate::prerequisites::InputSourceCandidate> = serde_json::from_slice(&serde_json::to_vec(&candidates).unwrap()).unwrap();
            prop_assert_eq!(wire, candidates);
            let free = [CapabilityRef { catalog: "matrix".into(), capability: "free".into() }];
            prop_assert!(project_input_source_candidates(&catalogs, &free, &permitted).unwrap().is_empty());
        }
    }
}
