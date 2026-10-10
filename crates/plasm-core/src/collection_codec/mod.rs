//! Recording codec for ordered collections. Evidence and rows are inseparable.
//!
//! Producer observations are trusted adapter facts, not claims accepted from Python.
//! Frames are for trusted persistence: their digest detects corruption, not forgery.
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
mod acquisition;
pub use acquisition::{Acquisition, PageTermination};
mod rows;
pub use rows::{RowIter, SharedRows};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ResultCoverage {
    Complete,
    Partial,
    #[default]
    Unknown,
}
impl ResultCoverage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Unknown => "unknown",
        }
    }
    /// Conservative dependency summary, not an operator transfer rule.
    pub fn combine(self, other: Self) -> Self {
        match (self, other) {
            (Self::Partial, _) | (_, Self::Partial) => Self::Partial,
            (Self::Unknown, _) | (_, Self::Unknown) => Self::Unknown,
            _ => Self::Complete,
        }
    }
    pub fn combine_all(iter: impl IntoIterator<Item = Self>) -> Self {
        iter.into_iter()
            .reduce(Self::combine)
            .unwrap_or(Self::Unknown)
    }
}
impl std::fmt::Display for ResultCoverage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollectionIdentity {
    pub catalog: [u8; 32],
    /// Canonical expression, selection and scoped occurrence fingerprint.
    pub expression: [u8; 32],
    pub epoch: u64,
}

impl CollectionIdentity {
    /// Value-only ingress has no catalogue authority. Its namespace is separate
    /// from every CGS digest and its identity binds the actual ingress context.
    pub fn for_untyped_observation(context: &impl Serialize) -> Result<Self, CollectionFault> {
        Ok(Self {
            catalog: Sha256::digest(b"plasm.untyped.observation.namespace.v1").into(),
            expression: Sha256::digest(serde_json::to_vec(context).map_err(|source| {
                CollectionFault::ObservationJson {
                    source: source.into(),
                }
            })?)
            .into(),
            epoch: 0,
        })
    }

    /// A derived expression belongs to this observation and records its operator.
    pub fn derived(&self, operator: &impl Serialize) -> Result<Self, CollectionFault> {
        let mut hash = Sha256::new();
        hash.update(b"plasm.collection.derivation.v1");
        hash.update(self.expression);
        hash.update(serde_json::to_vec(operator).map_err(|source| {
            CollectionFault::DerivationJson {
                source: source.into(),
            }
        })?);
        Ok(Self {
            catalog: self.catalog,
            expression: hash.finalize().into(),
            epoch: self.epoch,
        })
    }

    /// A derived expression is bound to its ordered input observations. Different
    /// catalogs and epochs are valid explicit dependencies, never interchangeable.
    pub fn with_inputs<'a>(mut self, inputs: impl IntoIterator<Item = &'a Self>) -> Self {
        let mut hash = Sha256::new();
        hash.update(b"plasm.collection.inputs.v1");
        hash.update(self.expression);
        for input in inputs {
            hash.update(input.catalog);
            hash.update(input.expression);
            hash.update(input.epoch.to_le_bytes());
        }
        self.expression = hash.finalize().into();
        self
    }

    /// Bind producer evidence to the pinned catalog and the actual expression,
    /// including resolved receiver/arguments, at the observation epoch.
    pub fn for_expression(
        cgs: &crate::CGS,
        expression: &impl Serialize,
        epoch: u64,
    ) -> Result<Self, CollectionFault> {
        let mut catalog = [0; 32];
        hex::decode_to_slice(cgs.catalog_cgs_hash_hex(), &mut catalog)
            .map_err(|source| CollectionFault::CatalogDigestHex { source })?;
        let bytes =
            serde_json::to_vec(expression).map_err(|source| CollectionFault::ExpressionJson {
                source: source.into(),
            })?;
        Ok(Self {
            catalog,
            expression: Sha256::digest(bytes).into(),
            epoch,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Gap {
    UndeclaredMembership,
    UnprovenTermination,
    OmittedOccurrences,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceGap {
    pub source: CollectionIdentity,
    pub reason: Gap,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Evidence {
    coverage: ResultCoverage,
    gaps: BTreeSet<EvidenceGap>,
}

/// Immutable ordered occurrences and their evidence have one owner. No mutable
/// row view or public deserializer can invalidate the recorded proof.
#[derive(Debug, PartialEq, Eq)]
pub struct RecordedCollection<R> {
    identity: CollectionIdentity,
    rows: SharedRows<R>,
    evidence: std::sync::Arc<Evidence>,
}
impl<R> Clone for RecordedCollection<R> {
    fn clone(&self) -> Self {
        Self {
            identity: self.identity.clone(),
            rows: self.rows.clone(),
            evidence: self.evidence.clone(),
        }
    }
}
impl<R: Serialize> Serialize for RecordedCollection<R> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("RecordedCollection", 3)?;
        state.serialize_field("identity", &self.identity)?;
        state.serialize_field("rows", &self.rows)?;
        state.serialize_field("evidence", self.evidence.as_ref())?;
        state.end()
    }
}
impl<R> RecordedCollection<R> {
    fn from_parts(identity: CollectionIdentity, rows: SharedRows<R>, evidence: Evidence) -> Self {
        Self {
            identity,
            rows,
            evidence: std::sync::Arc::new(evidence),
        }
    }
    pub fn identity(&self) -> &CollectionIdentity {
        &self.identity
    }
    pub fn observed(&self) -> &SharedRows<R> {
        &self.rows
    }
    pub fn coverage(&self) -> ResultCoverage {
        self.evidence.coverage
    }
    pub fn gaps(&self) -> &BTreeSet<EvidenceGap> {
        &self.evidence.gaps
    }
    /// Consume the record at a value-only boundary, discarding its evidence.
    pub fn into_rows(self) -> SharedRows<R> {
        self.rows
    }
}

/// Trusted-storage envelope. Restoring always verifies the codec frame and its
/// observation identity; deserializing this envelope alone grants no proof.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollectionCheckpoint {
    identity: CollectionIdentity,
    frame: Vec<u8>,
}
impl CollectionCheckpoint {
    pub fn capture<R>(
        codec: &dyn CollectionCodec<Row = R>,
        record: &RecordedCollection<R>,
    ) -> Result<Self, CollectionFault>
    where
        R: PartialEq + Serialize + DeserializeOwned,
    {
        Ok(Self {
            identity: record.identity().clone(),
            frame: codec.encode(record)?,
        })
    }
    pub fn restore<R>(
        &self,
        codec: &dyn CollectionCodec<Row = R>,
    ) -> Result<RecordedCollection<R>, CollectionFault>
    where
        R: PartialEq + Serialize + DeserializeOwned,
    {
        codec.decode(&self.frame, &self.identity)
    }
}

/// Adapter observations after raw-path/driver validation. Missing and malformed
/// declared exhaustive paths are errors at that adapter; they are never empty lists.
#[derive(Debug, Clone, Copy)]
pub enum Observation {
    Literal,
    /// All rows of an exact operation output (for example a singleton Get).
    ExactOutput {
        decoded: usize,
    },
    Exhausted {
        decoded: usize,
        discarded: usize,
    },
    Embedded {
        declared_exhaustive: bool,
        observed: usize,
    },
    UnprovenPage,
    MoreAvailable,
}

/// Validate an ordered materialized prefix against the producer's recorded
/// occurrences. Comparing values is intentional: callers supply semantic row
/// identities, not display strings or hydration fields. Duplicates remain
/// separate occurrences. A host cap may omit only a suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OccurrencePrefix {
    decoded: usize,
    discarded: usize,
}
impl OccurrencePrefix {
    pub fn discarded(self) -> usize {
        self.discarded
    }
    pub fn decoded(self) -> usize {
        self.decoded
    }
}

fn validate_occurrence_prefix<'a, 'b, R: PartialEq + 'a + 'b>(
    expected: impl IntoIterator<Item = &'a R>,
    retained: impl IntoIterator<Item = &'b R>,
) -> Result<OccurrencePrefix, CollectionFault> {
    let mut expected = expected.into_iter();
    let mut decoded = 0usize;
    for row in retained {
        if expected.next() != Some(row) {
            return Err(CollectionFault::Conservation);
        }
        decoded = decoded
            .checked_add(1)
            .ok_or(CollectionFault::Conservation)?;
    }
    let mut discarded = 0usize;
    for _ in expected {
        discarded = discarded
            .checked_add(1)
            .ok_or(CollectionFault::Conservation)?;
    }
    decoded = decoded
        .checked_add(discarded)
        .ok_or(CollectionFault::Conservation)?;
    Ok(OccurrencePrefix { decoded, discarded })
}

pub enum Transform<'a, R> {
    /// Completed typed evaluation. All declared dependencies contribute evidence.
    Evaluate {
        rows: Vec<R>,
    },
    /// One output occurrence per source occurrence, with new observed values.
    Map {
        rows: SharedRows<R>,
    },
    /// One changed row observation; all other payloads remain shared.
    Replace {
        index: usize,
        row: R,
    },
    Identity,
    /// A total permutation recorded by a sort operator. Values and duplicate
    /// occurrences are retained; the codec does not evaluate ordering keys.
    Reorder {
        positions: &'a [usize],
    },
    /// Retained source positions recorded by the operator; predicate execution stays outside the codec.
    Filter {
        retained: &'a [usize],
        captures: &'a [CollectionIdentity],
    },
    Distinct,
    Take(usize),
    Concat,
    /// Inputs are parent first, then one child per parent occurrence.
    FlatMap {
        children: &'a [CollectionIdentity],
    },
}

#[derive(Debug, Clone, Copy)]
pub enum Demand {
    Observed,
    Whole,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum CollectionFault {
    #[error("collection occurrence conservation failed")]
    Conservation,
    #[error("recorded collection payload is not resident")]
    NotResident,
    #[error("collection operation has invalid input arity")]
    Arity,
    #[error("scoped collection input {index} differs from its declared occurrence")]
    InputMismatch { index: usize },
    #[error("stored collection identity differs from requested identity")]
    IdentityMismatch,
    #[error("This operation requires a complete collection; coverage is {coverage}. {correction}", correction = incomplete_collection_correction(.gaps))]
    Incomplete {
        identity: CollectionIdentity,
        coverage: ResultCoverage,
        gaps: BTreeSet<EvidenceGap>,
    },
    #[error("collection observation cannot be encoded as JSON")]
    ObservationJson {
        #[source]
        source: std::sync::Arc<serde_json::Error>,
    },
    #[error("collection derivation cannot be encoded as JSON")]
    DerivationJson {
        #[source]
        source: std::sync::Arc<serde_json::Error>,
    },
    #[error("collection expression cannot be encoded as JSON")]
    ExpressionJson {
        #[source]
        source: std::sync::Arc<serde_json::Error>,
    },
    #[error("collection catalog digest is not valid hex")]
    CatalogDigestHex {
        #[source]
        source: hex::FromHexError,
    },
    #[error("collection frame payload cannot be encoded as JSON")]
    FrameEncodeJson {
        #[source]
        source: std::sync::Arc<serde_json::Error>,
    },
    #[error("collection frame payload is not valid collection JSON")]
    FrameDecodeJson {
        #[source]
        source: std::sync::Arc<serde_json::Error>,
    },
    #[error("collection frame is too short: {actual} bytes; at least 36 required")]
    FrameTooShort { actual: usize },
    #[error("collection frame magic or version is unsupported")]
    FrameHeader,
    #[error("collection frame digest does not match its payload")]
    FrameDigestMismatch,
    #[error("collection frame coverage and gaps are inconsistent")]
    FrameEvidenceInconsistent,
}

/// Render each distinct missing proof once. Keep occurrence identities and the
/// full evidence set in the typed fault, rather than the correction channel.
fn incomplete_collection_correction(gaps: &BTreeSet<EvidenceGap>) -> String {
    let reasons: BTreeSet<_> = gaps.iter().map(|gap| gap.reason).collect();
    if reasons.is_empty() {
        return "Use a source with proven complete coverage before counting, aggregating or claiming absence.".into();
    }
    reasons
        .into_iter()
        .map(|reason| match reason {
            Gap::UndeclaredMembership => "The response did not establish complete membership; re-observe the declared source before a whole-collection calculation.",
            Gap::UnprovenTermination => "Pagination has not proven exhaustion; finish fetching through a declared terminal page.",
            Gap::OmittedOccurrences => "Some occurrences were omitted; use the unabridged source before claiming a whole-collection result.",
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// One production evidence implementation. Adapters consume this interface rather
/// than implementing their own completeness checks. R remains a typed value.
pub trait CollectionCodec {
    type Row: PartialEq + Serialize + DeserializeOwned;
    /// Begin one scoped, ordered acquisition, whose final page can seal earlier pages.
    fn acquire(&self, identity: CollectionIdentity) -> Acquisition<Self::Row>;
    /// Check a materialized prefix without copying row identities. This proves
    /// occurrence conservation only; membership evidence comes from the producer.
    fn validate_prefix<'a, 'b>(
        &self,
        expected: &mut dyn Iterator<Item = &'a Self::Row>,
        retained: &mut dyn Iterator<Item = &'b Self::Row>,
    ) -> Result<OccurrencePrefix, CollectionFault>
    where
        Self::Row: 'a + 'b;
    fn record(
        &self,
        identity: CollectionIdentity,
        rows: Vec<Self::Row>,
        observation: Observation,
    ) -> Result<RecordedCollection<Self::Row>, CollectionFault>;
    fn derive(
        &self,
        identity: CollectionIdentity,
        inputs: &[&RecordedCollection<Self::Row>],
        operation: Transform<'_, Self::Row>,
    ) -> Result<RecordedCollection<Self::Row>, CollectionFault>;
    fn materialize<'a>(
        &self,
        collection: &'a RecordedCollection<Self::Row>,
        demand: Demand,
    ) -> Result<&'a SharedRows<Self::Row>, CollectionFault>;
    fn encode(
        &self,
        collection: &RecordedCollection<Self::Row>,
    ) -> Result<Vec<u8>, CollectionFault>;
    fn decode(
        &self,
        bytes: &[u8],
        expected: &CollectionIdentity,
    ) -> Result<RecordedCollection<Self::Row>, CollectionFault>;
}

#[derive(Debug, Default)]
pub struct RecordingCodec<R>(std::marker::PhantomData<fn() -> R>);
impl<R> RecordingCodec<R> {
    pub fn new() -> Self {
        Self(std::marker::PhantomData)
    }
}

impl<R: PartialEq + Serialize + DeserializeOwned> CollectionCodec for RecordingCodec<R> {
    type Row = R;
    fn acquire(&self, identity: CollectionIdentity) -> Acquisition<R> {
        Acquisition::new(identity)
    }
    fn validate_prefix<'a, 'b>(
        &self,
        expected: &mut dyn Iterator<Item = &'a R>,
        retained: &mut dyn Iterator<Item = &'b R>,
    ) -> Result<OccurrencePrefix, CollectionFault>
    where
        R: 'a + 'b,
    {
        validate_occurrence_prefix(expected, retained)
    }
    fn record(
        &self,
        identity: CollectionIdentity,
        rows: Vec<R>,
        observation: Observation,
    ) -> Result<RecordedCollection<R>, CollectionFault> {
        let (coverage, gap) = match observation {
            Observation::Literal => (ResultCoverage::Complete, None),
            Observation::ExactOutput { decoded } => {
                if decoded != rows.len() {
                    return Err(CollectionFault::Conservation);
                }
                (ResultCoverage::Complete, None)
            }
            Observation::Exhausted { decoded, discarded } => {
                if rows.len().checked_add(discarded) != Some(decoded) {
                    return Err(CollectionFault::Conservation);
                }
                if discarded == 0 {
                    (ResultCoverage::Complete, None)
                } else {
                    (ResultCoverage::Partial, Some(Gap::OmittedOccurrences))
                }
            }
            Observation::Embedded {
                declared_exhaustive,
                observed,
            } => {
                if observed != rows.len() {
                    return Err(CollectionFault::Conservation);
                }
                if declared_exhaustive {
                    (ResultCoverage::Complete, None)
                } else {
                    (ResultCoverage::Unknown, Some(Gap::UndeclaredMembership))
                }
            }
            Observation::UnprovenPage => (ResultCoverage::Unknown, Some(Gap::UnprovenTermination)),
            Observation::MoreAvailable => (ResultCoverage::Partial, Some(Gap::OmittedOccurrences)),
        };
        Ok(RecordedCollection::from_parts(
            identity.clone(),
            rows.into(),
            Evidence {
                coverage,
                gaps: gap
                    .into_iter()
                    .map(|reason| EvidenceGap {
                        source: identity.clone(),
                        reason,
                    })
                    .collect(),
            },
        ))
    }
    fn derive(
        &self,
        identity: CollectionIdentity,
        inputs: &[&RecordedCollection<R>],
        operation: Transform<'_, R>,
    ) -> Result<RecordedCollection<R>, CollectionFault> {
        let Some(first) = inputs.first() else {
            return Err(CollectionFault::Arity);
        };
        let identity = identity.with_inputs(inputs.iter().map(|input| input.identity()));
        let mut evidence = Evidence {
            coverage: ResultCoverage::combine_all(inputs.iter().map(|c| c.coverage())),
            gaps: inputs
                .iter()
                .flat_map(|c| c.gaps().iter().cloned())
                .collect(),
        };
        let rows = match operation {
            Transform::Evaluate { rows } => {
                if evidence.coverage != ResultCoverage::Complete {
                    evidence.coverage = ResultCoverage::Unknown;
                }
                rows.into()
            }
            Transform::Map { rows } => {
                if rows.len() != first.rows.len() {
                    return Err(CollectionFault::Conservation);
                }
                if inputs[1..]
                    .iter()
                    .any(|input| input.coverage() != ResultCoverage::Complete)
                {
                    evidence.coverage = ResultCoverage::Unknown;
                }
                rows
            }
            Transform::Concat => SharedRows::concat(inputs.iter().map(|c| &c.rows)),
            Transform::FlatMap { children } => {
                if first.rows.len().checked_add(1) != Some(inputs.len())
                    || children.len() != first.rows.len()
                {
                    return Err(CollectionFault::Arity);
                }
                for (index, (child, expected)) in inputs[1..].iter().zip(children).enumerate() {
                    if child.identity() != expected {
                        return Err(CollectionFault::InputMismatch { index });
                    }
                }
                // Unknown parent membership does not establish any unseen child.
                if first.coverage() != ResultCoverage::Complete {
                    evidence.coverage = ResultCoverage::Unknown;
                }
                SharedRows::concat(inputs[1..].iter().map(|c| &c.rows))
            }
            Transform::Filter { retained, captures } => {
                if inputs.len() != captures.len() + 1 {
                    return Err(CollectionFault::Arity);
                }
                for (index, (capture, expected)) in inputs[1..].iter().zip(captures).enumerate() {
                    if capture.identity() != expected {
                        return Err(CollectionFault::InputMismatch { index });
                    }
                }
                if retained.iter().any(|i| *i >= first.rows.len())
                    || retained.windows(2).any(|w| w[0] >= w[1])
                {
                    return Err(CollectionFault::Conservation);
                }
                if evidence.coverage != ResultCoverage::Complete {
                    evidence.coverage = ResultCoverage::Unknown;
                }
                first.rows.select(retained.iter().copied())?
            }
            op => {
                if inputs.len() != 1 {
                    return Err(CollectionFault::Arity);
                }
                match op {
                    Transform::Identity => first.rows.clone(),
                    Transform::Replace { index, row } => {
                        if index >= first.rows.len() {
                            return Err(CollectionFault::Conservation);
                        }
                        let before = first.rows.select(0..index)?;
                        let replacement = SharedRows::from(vec![row]);
                        let after = first.rows.select(index + 1..first.rows.len())?;
                        SharedRows::concat([&before, &replacement, &after])
                    }
                    Transform::Reorder { positions } => {
                        let mut seen = vec![false; first.rows.len()];
                        if positions.len() != seen.len() {
                            return Err(CollectionFault::Conservation);
                        }
                        for &position in positions {
                            let Some(visited) = seen.get_mut(position) else {
                                return Err(CollectionFault::Conservation);
                            };
                            if std::mem::replace(visited, true) {
                                return Err(CollectionFault::Conservation);
                            }
                        }
                        first.rows.select(positions.iter().copied())?
                    }
                    Transform::Distinct => {
                        if evidence.coverage != ResultCoverage::Complete {
                            evidence.coverage = ResultCoverage::Unknown;
                        }
                        let mut retained = Vec::new();
                        for (index, row) in first.rows.iter().enumerate() {
                            if !retained.iter().any(|&i| &first.rows[i] == row) {
                                retained.push(index);
                            }
                        }
                        first.rows.select(retained)?
                    }
                    Transform::Take(n) => {
                        if n == 0 {
                            evidence = Evidence {
                                coverage: ResultCoverage::Complete,
                                gaps: BTreeSet::new(),
                            };
                        } else if evidence.coverage != ResultCoverage::Complete {
                            evidence.coverage = ResultCoverage::Unknown;
                        }
                        first.rows.select(0..n.min(first.rows.len()))?
                    }
                    Transform::Evaluate { .. }
                    | Transform::Map { .. }
                    | Transform::Concat
                    | Transform::FlatMap { .. }
                    | Transform::Filter { .. } => return Err(CollectionFault::Arity),
                }
            }
        };
        Ok(RecordedCollection::from_parts(identity, rows, evidence))
    }
    fn materialize<'a>(
        &self,
        collection: &'a RecordedCollection<R>,
        demand: Demand,
    ) -> Result<&'a SharedRows<R>, CollectionFault> {
        if matches!(demand, Demand::Whole) && collection.coverage() != ResultCoverage::Complete {
            return Err(CollectionFault::Incomplete {
                identity: collection.identity.clone(),
                coverage: collection.coverage(),
                gaps: collection.gaps().clone(),
            });
        }
        Ok(&collection.rows)
    }
    fn encode(&self, collection: &RecordedCollection<R>) -> Result<Vec<u8>, CollectionFault> {
        let payload =
            serde_json::to_vec(collection).map_err(|source| CollectionFault::FrameEncodeJson {
                source: source.into(),
            })?;
        let mut frame = b"PCC\x01".to_vec();
        frame.extend_from_slice(&Sha256::digest(&payload));
        frame.extend(payload);
        Ok(frame)
    }
    fn decode(
        &self,
        bytes: &[u8],
        expected: &CollectionIdentity,
    ) -> Result<RecordedCollection<R>, CollectionFault> {
        if bytes.len() < 36 {
            return Err(CollectionFault::FrameTooShort {
                actual: bytes.len(),
            });
        }
        if &bytes[..4] != b"PCC\x01" {
            return Err(CollectionFault::FrameHeader);
        }
        let payload = &bytes[36..];
        if Sha256::digest(payload)[..] != bytes[4..36] {
            return Err(CollectionFault::FrameDigestMismatch);
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Stored<R> {
            identity: CollectionIdentity,
            rows: Vec<R>,
            evidence: Evidence,
        }
        let stored: Stored<R> =
            serde_json::from_slice(payload).map_err(|source| CollectionFault::FrameDecodeJson {
                source: source.into(),
            })?;
        let collection =
            RecordedCollection::from_parts(stored.identity, stored.rows.into(), stored.evidence);
        if &collection.identity != expected {
            return Err(CollectionFault::IdentityMismatch);
        }
        if (collection.coverage() == ResultCoverage::Complete) != collection.gaps().is_empty() {
            return Err(CollectionFault::FrameEvidenceInconsistent);
        }
        Ok(collection)
    }
}

#[cfg(test)]
mod tests;
