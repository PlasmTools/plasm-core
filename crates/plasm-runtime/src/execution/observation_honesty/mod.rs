//! Observation / partial-write honesty property suite (OPH).
//!
//! This module is the correctness gate for observation / partial-write honesty:
//!
//! | Layer | Generate | Check |
//! |---|---|---|
//! | Runtime proptest (here) | Op sequences, backend faults, barrier release schedules | Decode/hydrate/query vs independent [`ref_model`] |
//! | Shuttle (`plasm-agent-core`) | Fork/merge/commit interleavings | Shared-state CEP invariants (not HTTP internals) |
//!
//! HTTP completion order is driven explicitly via [`schedule::BarrierSchedule`] —
//! never `sleep`. See `docs/concurrent-execute-invariants.md` (**OPH-***).

#![cfg(test)]

mod hydrate_props;
mod partial_write;
mod ref_model;
mod schedule;
mod transport;

pub(crate) use ref_model::*;
pub(crate) use schedule::*;
pub(crate) use transport::*;
