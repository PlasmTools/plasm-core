//! Shared, iterative field/record dependency analysis. Classifiers retain the
//! existing contract laws; this driver owns dependency scheduling and memoization.
use super::{MapBodySchemaError, RecordUse};
use crate::python_compute::schema::PythonSchemaError;
use crate::{execute_session::ExecuteSession, plasm_plan::ValidatedPlanNode};
use plasm_core::value_contract::ValueContract;
use std::collections::{HashMap, HashSet};

#[derive(Clone, PartialEq, Eq, Hash)]
enum Request {
    Field { node: String, field: String },
    Record { node: String, row_operation: bool },
}

enum Failure {
    Field(PythonSchemaError),
    Record(MapBodySchemaError),
}

pub(crate) struct SchemaLookup<'a> {
    nodes: &'a [ValidatedPlanNode],
    by_id: HashMap<&'a str, usize>,
    complete: HashMap<Request, ValueContract>,
    active: HashSet<Request>,
    pending: Option<Request>,
}

impl<'a> SchemaLookup<'a> {
    pub(crate) fn node(&self, id: &str) -> Option<&'a ValidatedPlanNode> {
        self.by_id.get(id).map(|&index| &self.nodes[index])
    }

    pub(crate) fn field(
        &mut self,
        node: &str,
        field: &str,
    ) -> Result<ValueContract, PythonSchemaError> {
        let request = Request::Field {
            node: node.to_owned(),
            field: field.to_owned(),
        };
        if let Some(contract) = self.complete.get(&request) {
            return Ok(contract.clone());
        }
        if self.active.contains(&request) {
            return Err(PythonSchemaError::DependencyCycle);
        }
        if self.pending.is_none() {
            self.pending = Some(request);
        }
        Err(PythonSchemaError::DependencyPending)
    }

    pub(crate) fn record(
        &mut self,
        node: &str,
        row_operation: bool,
    ) -> Result<ValueContract, MapBodySchemaError> {
        let request = Request::Record {
            node: node.to_owned(),
            row_operation,
        };
        if let Some(contract) = self.complete.get(&request) {
            return Ok(contract.clone());
        }
        if self.active.contains(&request) {
            return Err(MapBodySchemaError::DependencyCycle);
        }
        if self.pending.is_none() {
            self.pending = Some(request);
        }
        Err(MapBodySchemaError::DependencyPending)
    }
}

fn analyze(
    es: &ExecuteSession,
    nodes: &[ValidatedPlanNode],
    root: Request,
) -> Result<ValueContract, Failure> {
    let mut lookup = SchemaLookup {
        nodes,
        by_id: nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.id().as_str(), index))
            .collect(),
        complete: HashMap::new(),
        active: HashSet::new(),
        pending: None,
    };
    let mut work = vec![root.clone()];
    while let Some(request) = work.last().cloned() {
        lookup.active.insert(request.clone());
        let result = match &request {
            Request::Field { node, field } => {
                crate::python_compute::schema::classify_source_field_kind(
                    es,
                    nodes,
                    node,
                    field,
                    &mut lookup,
                )
                .map_err(Failure::Field)
            }
            Request::Record {
                node,
                row_operation,
            } => super::classify_record_contract(
                es,
                nodes,
                node,
                if *row_operation {
                    RecordUse::RowOperation
                } else {
                    RecordUse::Value
                },
                &mut lookup,
            )
            .map_err(Failure::Record),
        };
        if let Some(dependency) = lookup.pending.take() {
            work.push(dependency);
            continue;
        }
        // A real dependency failure terminates analysis at its typed cause.
        // It is never wrapped once per ancestor or treated as a guessed contract.
        let contract = result?;
        lookup.complete.insert(request.clone(), contract);
        lookup.active.remove(&request);
        work.pop();
    }
    Ok(lookup
        .complete
        .remove(&root)
        .expect("completed root contract"))
}

pub(crate) fn field(
    es: &ExecuteSession,
    nodes: &[ValidatedPlanNode],
    node: &str,
    field: &str,
) -> Result<ValueContract, PythonSchemaError> {
    analyze(
        es,
        nodes,
        Request::Field {
            node: node.to_owned(),
            field: field.to_owned(),
        },
    )
    .map_err(|failure| match failure {
        Failure::Field(error) => error,
        Failure::Record(error) => error.into(),
    })
}

pub(super) fn record(
    es: &ExecuteSession,
    nodes: &[ValidatedPlanNode],
    node: &str,
    row_operation: bool,
) -> Result<ValueContract, MapBodySchemaError> {
    analyze(
        es,
        nodes,
        Request::Record {
            node: node.to_owned(),
            row_operation,
        },
    )
    .map_err(|failure| match failure {
        Failure::Record(error) => error,
        Failure::Field(error) => error.into(),
    })
}
