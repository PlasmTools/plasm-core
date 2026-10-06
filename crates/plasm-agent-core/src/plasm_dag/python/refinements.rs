//! Monty branch contracts mapped onto immutable DAG references. No predicate
//! syntax is interpreted here, and evidence remains local to its branch.
use super::*;
use plasm_core::value_contract::ValueContract;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Reference {
    root: String,
    path: Vec<String>,
}
impl Reference {
    pub(super) fn of(expr: &PyExpr) -> Option<Self> {
        match expr {
            PyExpr::Name(n) => Some(Self {
                root: n.id.to_string(),
                path: Vec::new(),
            }),
            PyExpr::Attribute(a) => {
                let mut reference = Self::of(&a.value)?;
                reference.path.push(a.attr.to_string());
                Some(reference)
            }
            _ => None,
        }
    }
}

pub(super) type Facts = BTreeMap<Reference, ValueContract>;

impl Lower<'_> {
    fn canonical_reference(&self, mut reference: Reference) -> Reference {
        if let Some(binding) = self.frame.names.get(&reference.root) {
            reference.root = binding.clone();
        }
        if let Some(node) = self.state.get(&reference.root) {
            reference.root = node.id.clone();
        }
        reference
    }

    pub(super) fn reference(&self, expression: &PyExpr) -> Option<Reference> {
        Reference::of(expression).map(|reference| self.canonical_reference(reference))
    }

    /// Reapply facts to the immutable captured port, including observations of
    /// its fields. Capturing a receiver must not discard a proven field contract.
    pub(super) fn refine_capture(
        &self,
        reference: &Reference,
        value: PlasmDataValue,
        inputs: &BTreeMap<String, crate::plasm_plan::PlanDataInput>,
    ) -> Result<PlasmDataValue, PythonLoweringError> {
        fn refine_at(
            contract: &mut ValueContract,
            path: &[String],
            evidence: &ValueContract,
            depth: usize,
        ) -> Result<(), PythonLoweringError> {
            use plasm_core::value_contract::ValueShape;
            if depth >= 64 {
                return Err(
                    crate::program_rejection::PythonProgramError::CaptureRefinementTooDeep.into(),
                );
            }
            if let Some((name, rest)) = path.split_first() {
                match &mut contract.shape {
                    ValueShape::Record { fields } | ValueShape::ObservedRecord { fields, .. } => {
                        if let Some(field) = fields.get_mut(name) {
                            refine_at(field, rest, evidence, depth + 1)?;
                        }
                    }
                    ValueShape::Union { variants } => {
                        for variant in variants {
                            refine_at(variant, path, evidence, depth + 1)?;
                        }
                    }
                    _ => {}
                }
            } else {
                *contract = contract.refined_by(evidence)?;
            }
            Ok(())
        }
        let mut facts = self
            .frame
            .facts
            .iter()
            .filter(|(observed, _)| {
                observed.root == reference.root && observed.path.starts_with(&reference.path)
            })
            .peekable();
        if facts.peek().is_none() {
            return Ok(value);
        }
        let original = self.value_type(&value, inputs)?;
        let mut contract = original.clone();
        for (observed, evidence) in facts {
            refine_at(
                &mut contract,
                &observed.path[reference.path.len()..],
                evidence,
                0,
            )?;
        }
        if contract == original {
            return Ok(value);
        }
        Ok(PlasmDataValue::Expression {
            expression: plasm_core::value_expression::ValueOperation::Refine {
                value: Box::new(value),
                contract,
            },
        })
    }

    fn branch_facts(
        &mut self,
        condition: &PyExpr,
        selected: bool,
    ) -> Result<Facts, PythonLoweringError> {
        let saved = (self.state.clone(), self.serial, self.spans.clone());
        let result = (|| {
            let mut inputs = BTreeMap::new();
            let captured = self.capture_expression(condition, &mut inputs)?;
            let mut fields = BTreeMap::new();
            let bindings: BTreeMap<_, _> = captured
                .references
                .iter()
                .map(|(reference, name)| (name.clone(), reference.clone()))
                .collect();
            for (name, value) in &captured.fields {
                fields.insert(name.clone(), self.value_type(value, &inputs)?);
            }
            let input = ValueContract::record(fields, Default::default());
            let paths = crate::python_compute::observation_paths(&input)?;
            let source = format!(
                "{}\n@compute\ndef predicate(__plasm_input: Row):\n    return ({})\n",
                self.imports.source,
                monty::expression_source(&captured.expression)
            );
            let Some(types) = crate::python_compute::branch_contracts(&source, &input, &paths)?
            else {
                return Ok(Facts::new());
            };
            let mut facts = Facts::new();
            for (path, (yes, no)) in paths.into_iter().zip(types) {
                let Some((first, rest)) = path.split_first() else {
                    continue;
                };
                if let Some(binding) = bindings.get(first) {
                    let mut reference = binding.clone();
                    reference.path.extend_from_slice(rest);
                    facts.insert(reference, if selected { yes } else { no });
                }
            }
            Ok(facts)
        })();
        (self.state, self.serial, self.spans) = saved;
        result
    }
    pub(super) fn under_condition<T>(
        &mut self,
        condition: &PyExpr,
        selected: bool,
        lower: impl FnOnce(&mut Self) -> Result<T, PythonLoweringError>,
    ) -> Result<T, PythonLoweringError> {
        let facts = self.branch_facts(condition, selected)?;
        let previous = self.frame.facts.clone();
        self.frame.facts.extend(facts);
        let result = lower(self);
        self.frame.facts = previous;
        result
    }
}
