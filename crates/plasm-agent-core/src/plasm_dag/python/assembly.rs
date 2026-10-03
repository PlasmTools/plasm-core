//! Assemble typed row values and explicit singleton dependencies using ordinary derivation.
use super::*;

impl Lower<'_> {
    /// A record is a value, not an implicit traversal of any of its inputs.
    /// Outside a row scope, derive over the unit row so empty embedded arrays
    /// still produce one record and missing singleton fields fail explicitly.
    pub(super) fn record_value(
        &mut self,
        site: &PyExpr,
        id: &str,
    ) -> Result<String, PythonLoweringError> {
        let mut inputs = BTreeMap::new();
        let value = self.scoped_value(site, &mut inputs)?;
        self.emit_value(value, inputs.into_values().collect(), id)
    }

    pub(super) fn emit_value(
        &mut self,
        value: PlasmDataValue,
        inputs: Vec<crate::plasm_plan::PlanDataInput>,
        id: &str,
    ) -> Result<String, PythonLoweringError> {
        let source = match &self.frame.row {
            Some(source) => source.clone(),
            None => {
                let unit = self.fresh();
                self.insert(DagNode {
                    id: unit,
                    expr: String::new(),
                    singleton: true,
                    page_size: None,
                    source: super::super::types::DagNodeSource::Data(PlanValue::Literal {
                        value: plasm_core::Value::Object(Default::default()).try_into()?,
                    }),
                })?
            }
        };
        let singleton = self.state.get(&source).is_some_and(|node| node.singleton);
        self.insert(DagNode {
            id: id.into(),
            expr: String::new(),
            singleton,
            page_size: None,
            source: super::super::types::DagNodeSource::Derive {
                value_type: None,
                source,
                value,
                inputs,
            },
        })?;
        let value_type = match &self
            .state
            .get(id)
            .ok_or("missing constructed value")?
            .source
        {
            super::super::types::DagNodeSource::Derive {
                value,
                source,
                inputs,
                ..
            } => super::text::derive_contract(self.es, &self.state, source, value, inputs, 0)?,
            _ => unreachable!(),
        };
        let index = self.state.labels[id];
        let super::super::types::DagNodeSource::Derive {
            value_type: target, ..
        } = &mut std::sync::Arc::make_mut(&mut self.state.nodes[index]).source
        else {
            unreachable!("record construction inserted a derivation")
        };
        *target = Some(value_type);
        Ok(id.into())
    }
}
