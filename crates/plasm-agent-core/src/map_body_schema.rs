//! Catalog-pinned recursive output typing for correlated records.
use crate::execute_session::ExecuteSession;
use plasm_core::plasm_monad::*;
use plasm_core::value_contract::ValueContract;

pub(crate) fn output_schema(
    es: &ExecuteSession,
    body: &CorrelatedBody,
) -> Result<SyntheticResultSchema, String> {
    let PlasmReturn::Step { step } = &body.body.return_ else {
        return Err("expected one return".into());
    };
    let Some(PlasmStepPayload::Derive(output)) = body.body.steps.get(step.as_str()) else {
        return Err("correlated slice requires an object derivation as its output".into());
    };
    let PlasmDataValue::Object { fields } = &output.derive.value else {
        return Err("correlated output must be a typed object".into());
    };
    if output.derive.source.as_deref() != Some(body.parent.local.as_str()) || fields.is_empty() {
        return Err(
            "correlated output must map its captured singleton into a nonempty object".into(),
        );
    }
    let cgs = &es
        .contexts_by_entry
        .get(&body.parent.entity.entry_id)
        .ok_or("capture catalog not loaded")?
        .cgs;
    let entity = cgs
        .get_entity(&body.parent.entity.entity)
        .ok_or("capture entity missing")?;
    let mut schema = Vec::new();
    for (name, value) in fields {
        let value_type = ValueContract::data_value(value, &mut |binding, path| {
            if output.derive.item_binding.as_ref().map(|b| b.as_str()) == Some(binding)
                && path.len() == 1
            {
                let field = entity
                    .fields
                    .get(path[0].as_str())
                    .ok_or("unknown captured field")?;
                let mut t = ValueContract::from_domain(
                    cgs,
                    &body.parent.entity.entry_id,
                    field.kind.registry_key(),
                )?;
                t.nullable = !field.required;
                return Ok(t);
            }
            if path == ["content"]
                && matches!(body.body.steps.get(binding), Some(PlasmStepPayload::Map(m)) if matches!(m.compute.op, ComputeOp::Python { .. }))
            {
                return Ok(ValueContract::scalar(plasm_core::FieldType::String));
            }
            Err("correlated output references an untyped dependency".into())
        })?;
        schema.push(SyntheticFieldSchema {
            name: OutputName::new(name.clone())?,
            value_kind: value_type.summary(),
            value_type: Some(value_type),
            source: None,
        });
    }
    Ok(SyntheticResultSchema {
        entity: None,
        fields: schema,
    })
}
