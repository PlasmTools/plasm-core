//! Discovery inputs are derived from the declared prerequisite contract.
//! This projection does not alter invocation typing, teaching, or acquisition closure.

use crate::prerequisites::{InputLane, InputPath};
use crate::schema::{
    CapabilityInputs, CapabilitySchema, InputFieldSchema, InputFieldWire, InputType,
};

pub(super) fn relevance_inputs(
    cgs: &crate::CGS,
    capability: &CapabilitySchema,
) -> Result<CapabilityInputs, String> {
    let mut inputs = capability.inputs.clone();
    for path in cgs.prerequisites.acquired_inputs(capability.name.as_str()) {
        // A derived read's identity seat is implicit, not an authored input field.
        if path.lane == InputLane::Selection
            && capability
                .derived
                .as_ref()
                .is_some_and(|derived| path.path == [derived.identity_field.clone()])
        {
            continue;
        }
        remove_acquired(&mut inputs, path).map_err(|error| {
            format!(
                "invalid discovery acquisition binding on {}: {error}",
                capability.name
            )
        })?;
    }
    Ok(inputs)
}

fn remove_acquired(inputs: &mut CapabilityInputs, input: &InputPath) -> Result<(), String> {
    let fields = match input.lane {
        InputLane::Scope => &mut inputs.scope.0,
        InputLane::Selection => &mut inputs.selection.0,
        InputLane::Controls => &mut inputs.controls.0,
        InputLane::Arguments | InputLane::Payload => {
            let lane = if input.lane == InputLane::Arguments {
                &mut inputs.arguments
            } else {
                &mut inputs.payload
            };
            let schema = lane.as_mut().ok_or("missing acquired input lane")?;
            let InputType::Object {
                fields,
                additional_fields,
            } = &mut schema.input_type
            else {
                return Err("acquired input path requires an object lane".into());
            };
            remove_field(fields, &input.path)?;
            if fields.is_empty() && !*additional_fields {
                *lane = None;
            }
            return Ok(());
        }
    };
    remove_field(fields, &input.path)
}

fn remove_field(fields: &mut Vec<InputFieldSchema>, path: &[String]) -> Result<(), String> {
    let (head, rest) = path.split_first().ok_or("empty acquired input path")?;
    let index = fields
        .iter()
        .position(|field| &field.name == head)
        .ok_or_else(|| format!("missing acquired field {head}"))?;
    if rest.is_empty() {
        fields.remove(index);
    } else {
        let InputFieldWire::Inline(ty) = &mut fields[index].wire else {
            return Err("acquired input path traverses a scalar".into());
        };
        let InputType::Object {
            fields: children,
            additional_fields,
        } = ty.as_mut()
        else {
            return Err("acquired input path traverses a non-object".into());
        };
        remove_field(children, rest)?;
        if children.is_empty() && !*additional_fields {
            fields.remove(index);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{InputSchema, InputValidation};

    fn fixture() -> crate::CGS {
        crate::load_schema(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/prerequisite_matrix"),
        )
        .unwrap()
    }

    fn object(fields: Vec<InputFieldSchema>) -> InputSchema {
        InputSchema {
            input_type: InputType::Object {
                fields,
                additional_fields: false,
            },
            validation: InputValidation::default(),
            description: None,
            examples: Vec::new(),
        }
    }

    fn configure(lane: InputLane, name: &str, business_name: &str) -> crate::CGS {
        let mut cgs = fixture();
        let cap = cgs.capabilities.get_mut("read").unwrap();
        let mut acquired = cap.inputs.selection.0[0].clone();
        acquired.name = name.into();
        acquired.description = Some("Acquisition-only sentinel".into());
        let mut business = acquired.clone();
        business.name = business_name.into();
        business.description = Some("Ordinary business sentinel".into());
        cap.inputs = CapabilityInputs::default();
        let fields = vec![acquired, business];
        match lane {
            InputLane::Scope => cap.inputs.scope.0 = fields,
            InputLane::Selection => cap.inputs.selection.0 = fields,
            InputLane::Controls => cap.inputs.controls.0 = fields,
            InputLane::Arguments => cap.inputs.arguments = Some(object(fields)),
            InputLane::Payload => cap.inputs.payload = Some(object(fields)),
        }
        cgs.prerequisites.requirements.get_mut("read").unwrap()[0].bindings[0].input = InputPath {
            lane,
            path: vec![name.into()],
        };
        cgs
    }

    proptest::proptest! {
        #[test]
        fn acquisition_is_exact_model_evidence_not_name_or_value_type(
            name in "seat_[a-z]{1,20}", lane in 0usize..5, misleading_name in 0usize..3,
        ) {
            let lane = [InputLane::Scope, InputLane::Selection, InputLane::Controls, InputLane::Arguments, InputLane::Payload][lane].clone();
            let business_name = ["access_token", "password", "secret"][misleading_name];
            let cgs = configure(lane, &name, business_name);
            let before = serde_json::to_vec(&cgs).unwrap();
            let documents = super::super::capability_documents(&cgs).unwrap();
            let doc = documents.iter().find(|d| d.capability == "read").unwrap();
            proptest::prop_assert!(!doc.text.contains("Acquisition-only sentinel"));
            proptest::prop_assert!(!doc.operation.contract.contains("Acquisition-only sentinel"));
            proptest::prop_assert!(doc.text.contains("Ordinary business sentinel"));
            proptest::prop_assert!(doc.operation.contract.contains("Ordinary business sentinel"));
            // The same named value is used by both inputs. It is not acquisition proof.
            proptest::prop_assert_eq!(serde_json::to_vec(&cgs).unwrap(), before.clone());
            let decoded: crate::CGS = serde_json::from_slice(&before).unwrap();
            proptest::prop_assert_eq!(&documents, &super::super::capability_documents(&decoded).unwrap());
            let mut unbound = cgs.clone();
            unbound.prerequisites.requirements.remove("read");
            let ordinary = super::super::capability_documents(&unbound).unwrap();
            let ordinary = ordinary.iter().find(|d| d.capability == "read").unwrap();
            proptest::prop_assert!(ordinary.text.contains("Acquisition-only sentinel"));
            proptest::prop_assert_ne!(&doc.text_hash, &ordinary.text_hash);
        }
    }

    #[test]
    fn identical_names_in_other_lanes_are_not_acquired() {
        let mut cgs = configure(InputLane::Selection, "opaque", "secret");
        let mut other = cgs.capabilities["read"].inputs.selection.0[0].clone();
        other.description = Some("Different lane remains ordinary".into());
        cgs.capabilities.get_mut("read").unwrap().inputs.arguments = Some(object(vec![other]));
        let documents = super::super::capability_documents(&cgs).unwrap();
        let doc = documents.iter().find(|d| d.capability == "read").unwrap();
        assert!(!doc.operation.contract.contains("Acquisition-only sentinel"));
        assert!(doc
            .operation
            .contract
            .contains("Different lane remains ordinary"));
    }

    #[test]
    fn nested_acquisitions_use_the_whole_path_and_leave_siblings() {
        let mut cgs = configure(InputLane::Payload, "opaque", "secret");
        let cap = cgs.capabilities.get_mut("read").unwrap();
        let InputType::Object { fields, .. } = &cap.inputs.payload.as_ref().unwrap().input_type
        else {
            unreachable!()
        };
        let mut left = fields[0].clone();
        left.name = "left".into();
        left.description = None;
        left.wire = InputFieldWire::Inline(Box::new(InputType::Object {
            fields: vec![fields[0].clone()],
            additional_fields: false,
        }));
        let mut right = left.clone();
        right.name = "right".into();
        let InputFieldWire::Inline(ty) = &mut right.wire else {
            unreachable!()
        };
        let InputType::Object { fields, .. } = ty.as_mut() else {
            unreachable!()
        };
        fields[0].description = Some("Unbound sibling sentinel".into());
        cap.inputs.payload = Some(object(vec![left, right]));
        cgs.prerequisites.requirements.get_mut("read").unwrap()[0].bindings[0]
            .input
            .path = vec!["left".into(), "opaque".into()];
        let documents = super::super::capability_documents(&cgs).unwrap();
        let doc = documents.iter().find(|d| d.capability == "read").unwrap();
        assert!(!doc.operation.contract.contains("Acquisition-only sentinel"));
        assert!(doc.operation.contract.contains("Unbound sibling sentinel"));
    }

    #[test]
    fn derived_identity_seat_is_not_mistaken_for_an_authored_field() {
        let mut cgs = fixture();
        let mut cap = cgs.capabilities["read"].clone();
        cap.name = "keyed_read".into();
        cap.kind = crate::CapabilityKind::Get;
        cap.inputs = CapabilityInputs::default();
        cap.mapping = None;
        cap.derived = Some(crate::DerivedGetPlan {
            get_capability: "keyed_read".into(),
            source_query: "read".into(),
            match_field: "id".into(),
            identity_field: "id".into(),
            projection: [("id".into(), "id".into())].into_iter().collect(),
        });
        let mut requirement = cgs.prerequisites.requirements["read"][0].clone();
        requirement.bindings[0].input.path = vec!["id".into()];
        cgs.prerequisites
            .requirements
            .insert("keyed_read".into(), vec![requirement]);
        cgs.capabilities.insert(cap.name.clone(), cap);
        let docs = super::super::capability_documents(&cgs).unwrap();
        assert!(docs.iter().any(|d| d.capability == "keyed_read"));
    }

    #[test]
    fn malformed_acquisition_fails_instead_of_guessing_a_seat() {
        let mut cgs = fixture();
        cgs.prerequisites.requirements.get_mut("read").unwrap()[0].bindings[0]
            .input
            .path = vec!["absent".into()];
        assert!(super::super::capability_documents(&cgs)
            .unwrap_err()
            .contains("missing input field absent"));
    }
}
