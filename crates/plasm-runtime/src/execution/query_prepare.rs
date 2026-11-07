//! Prepare one query request and its residual row predicate.
use super::*;

pub(super) struct PreparedQuery<'a> {
    pub capability: &'a CapabilitySchema,
    pub env: CmlEnv,
    pub bindings: IndexMap<String, Value>,
    pub residual: Option<Predicate>,
}

pub(super) fn prepare<'a>(
    query: &QueryExpr,
    cgs: &'a CGS,
) -> Result<PreparedQuery<'a>, RuntimeError> {
    let resolved = plasm_compile::predicate_compiler::resolve_query(query, cgs)
        .map_err(|source| RuntimeError::CompilationError { source })?;
    let mut env = CmlEnv::new();
    if let Some(filter) = &resolved.filter {
        env.insert("filter".into(), json_to_plasm_value(&filter.to_json()));
    }
    env.extend(resolved.bindings.clone());
    normalize_cml_env_inputs(&mut env, cgs, resolved.capability)?;
    plasm_core::apply_entity_ref_scope_splat(&mut env, cgs, resolved.capability)?;
    if let Some(projection) = &query.projection {
        env.insert(
            "projection".into(),
            Value::Array(projection.iter().cloned().map(Value::String).collect()),
        );
    }
    Ok(PreparedQuery {
        capability: resolved.capability,
        env,
        bindings: resolved.bindings,
        residual: resolved.residual,
    })
}
