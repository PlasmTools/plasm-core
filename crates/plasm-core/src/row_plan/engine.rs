//! Collected row values and their declared schema.

use crate::plasm_monad::StepId;
use crate::ValueRow;

use super::collect::CollectReason;
use super::schema::PlasmFrameSchema;

#[derive(Debug, Clone, PartialEq)]
pub struct CollectedFrame {
    pub schema: PlasmFrameSchema,
    pub rows: Vec<ValueRow>,
    /// Input occurrence retained by each output; synthesized rows have no witness.
    pub occurrences: Vec<Option<usize>>,
}

impl CollectedFrame {
    /// Validate the correspondence returned across the row-engine boundary.
    pub fn validate_correspondence(
        &self,
        input_len: usize,
    ) -> Result<(), super::error::RowCorrespondenceError> {
        if self.rows.len() != self.occurrences.len() {
            return Err(super::error::RowCorrespondenceError::LengthMismatch);
        }
        if self
            .occurrences
            .iter()
            .flatten()
            .any(|index| *index >= input_len)
        {
            return Err(super::error::RowCorrespondenceError::InputIndexOutOfBounds);
        }
        Ok(())
    }

    #[must_use]
    pub fn empty(schema: PlasmFrameSchema) -> Self {
        Self {
            schema,
            rows: Vec::new(),
            occurrences: Vec::new(),
        }
    }
}

impl CollectReason {
    #[must_use]
    pub fn program_return(step: StepId) -> Self {
        Self::ProgramReturn { step }
    }
}
