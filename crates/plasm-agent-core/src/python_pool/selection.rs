//! Opaque instance witnesses live for one worker invocation only. They never
//! enter record fields, Python source, persisted values, or model responses.
use super::*;
use monty_types::ObjectRef;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SelectedOccurrence {
    pub port: usize,
    pub index: usize,
}

pub(crate) struct ComputeOutput {
    pub value: plasm_core::Value,
    pub occurrences: Vec<Option<SelectedOccurrence>>,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum SelectionError {
    #[error("selection ingress is not a row collection")]
    RowsRequired,
    #[error("selection packet must contain exactly one row")]
    PacketCardinality,
    #[error("selection input port is absent")]
    PortAbsent,
    #[error("selection source contains a non-record occurrence")]
    RecordRequired,
    #[error(transparent)]
    Output(#[from] PythonOutputError),
}

pub(super) struct Witnesses {
    enabled: bool,
    records: BTreeMap<MontyUuid, (SelectedOccurrence, plasm_core::Value)>,
}

impl Witnesses {
    pub fn capture(input: ObjectRef<'_>, ports: &[Option<String>]) -> Result<Self, SelectionError> {
        let mut records = BTreeMap::new();
        if ports.is_empty() {
            return Ok(Self {
                records,
                enabled: false,
            });
        }
        let rows = input.items().ok_or(SelectionError::RowsRequired)?;
        for (port, field) in ports.iter().enumerate() {
            let candidates = if let Some(field) = field {
                let [row] = rows.as_slice() else {
                    return Err(SelectionError::PacketCardinality);
                };
                let value = row
                    .pairs()
                    .and_then(|pairs| {
                        pairs
                            .into_iter()
                            .find(|(key, _)| key.as_str() == Some(field.as_str()))
                            .map(|(_, value)| value)
                    })
                    .ok_or(SelectionError::PortAbsent)?;
                if value.type_name() == "list" {
                    value.items().ok_or(SelectionError::RowsRequired)?
                } else {
                    vec![value]
                }
            } else {
                rows.clone()
            };
            for (index, value) in candidates.into_iter().enumerate() {
                let monty_types::unstable::MontyNode::ClassInstance { instance_id, .. } =
                    monty_types::unstable::node(value)
                else {
                    return Err(SelectionError::RecordRequired);
                };
                let mut budget = MAX_OUTPUT;
                let snapshot = output_value(value, 0, &mut budget)?;
                records.insert(*instance_id, (SelectedOccurrence { port, index }, snapshot));
            }
        }
        Ok(Self {
            records,
            enabled: true,
        })
    }

    pub fn select(
        &self,
        output: ObjectRef<'_>,
    ) -> Result<Vec<Option<SelectedOccurrence>>, SelectionError> {
        if !self.enabled {
            return Ok(Vec::new());
        }
        let values = if output.type_name() == "list" {
            output.items().ok_or(SelectionError::RowsRequired)?
        } else {
            vec![output]
        };
        values
            .into_iter()
            .map(|value| {
                let monty_types::unstable::MontyNode::ClassInstance { instance_id, .. } =
                    monty_types::unstable::node(value)
                else {
                    return Ok(None);
                };
                let Some((occurrence, before)) = self.records.get(instance_id) else {
                    return Ok(None);
                };
                // Equality verifies immutability only after opaque identity matched.
                // A copied record with equal fields never reaches this comparison.
                let mut budget = MAX_OUTPUT;
                let after = output_value(value, 0, &mut budget)?;
                Ok((before == &after).then_some(*occurrence))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(class: &MontyObject, id: MontyUuid, number: i64) -> MontyObject {
        MontyObject::class_instance(
            class.clone(),
            id,
            vec![(MontyObject::string("number"), MontyObject::int(number))],
        )
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(64))]
        #[test]
        fn equal_values_keep_distinct_occurrences_and_selection_order(count in 1usize..32, indices in proptest::collection::vec(0usize..128, 0..64)) {
            let class = MontyObject::record_type("Row", fresh_uuid());
            let rows = (0..count).map(|_| record(&class, fresh_uuid(), 7)).collect::<Vec<_>>();
            let input = MontyObject::list(rows.clone());
            let proof = Witnesses::capture(input.as_ref(), &[None]).unwrap();
            let selected = MontyObject::list(indices.iter().map(|index| rows[index % count].clone()));
            let expected = indices.iter().map(|index| Some(SelectedOccurrence { port: 0, index: index % count })).collect::<Vec<_>>();
            proptest::prop_assert_eq!(proof.select(selected.as_ref()).unwrap(), expected);
        }
    }

    #[test]
    fn copied_or_changed_records_have_no_witness() {
        let class = MontyObject::record_type("Row", fresh_uuid());
        let id = fresh_uuid();
        let input = MontyObject::list(vec![record(&class, id, 7)]);
        let proof = Witnesses::capture(input.as_ref(), &[None]).unwrap();
        for output in [record(&class, fresh_uuid(), 7), record(&class, id, 8)] {
            assert_eq!(proof.select(output.as_ref()).unwrap(), [None]);
        }
    }
}
