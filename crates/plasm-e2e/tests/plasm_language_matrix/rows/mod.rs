//! Matrix row tables (concatenated groups).

mod bound_reads;
mod core;
mod effects;
mod federated;
mod identities;

use super::row::MatrixRow;

pub(crate) fn all_rows() -> impl Iterator<Item = &'static MatrixRow> {
    core::ROWS
        .iter()
        .chain(effects::ROWS.iter())
        .chain(bound_reads::ROWS.iter())
        .chain(identities::ROWS.iter())
        .chain(federated::ROWS.iter())
}

pub(crate) fn row_count() -> usize {
    core::ROWS.len()
        + effects::ROWS.len()
        + federated::ROWS.len()
        + bound_reads::ROWS.len()
        + identities::ROWS.len()
}

pub(crate) fn find_row(id: &str) -> Option<&'static MatrixRow> {
    all_rows().find(|r| r.id == id)
}
