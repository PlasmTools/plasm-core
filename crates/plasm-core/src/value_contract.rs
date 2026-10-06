//! Recursive, catalog-pinned materialized types. Domain constraints remain owned by CGS.
use crate::{FieldType, ValueDomainKey, CGS};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

mod intersection;
mod refinement;
mod transfer;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainRef {
    pub entry_id: String,
    pub catalog_hash: String,
    pub value_ref: ValueDomainKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValueContract {
    pub shape: ValueShape,
    pub domain: Option<DomainRef>,
    pub nullable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ValueContractError {
    #[error("field `{name}` is absent from the record contract")]
    FieldMissing { name: String },
    #[error("field `{name}` requires a record contract")]
    FieldRequiresRecord { name: String },
    #[error("union contract has no variants")]
    EmptyUnion,
    #[error("nested observation depth exceeded the supported limit")]
    ObservationDepthExceeded,
    #[error("reference catalog `{entry_id}` is not loaded")]
    ReferenceCatalogNotLoaded { entry_id: String },
    #[error("reference target `{target}` is absent from its catalog")]
    ReferenceTargetMissing { target: String },
    #[error("observation matches no union variant")]
    NoUnionVariantMatches,
    #[error("observed union projection is ambiguous")]
    AmbiguousUnionProjection,
    #[error("value nesting exceeded the supported limit")]
    ValidationDepthExceeded,
    #[error("value domain catalog `{entry_id}` is not loaded")]
    DomainCatalogNotLoaded { entry_id: String },
    #[error("value domain catalog pin does not match for `{entry_id}`")]
    DomainCatalogPinMismatch { entry_id: String },
    #[error("value domain `{value_ref}` is absent from catalog `{entry_id}`")]
    DomainMissing { entry_id: String, value_ref: String },
    #[error("expected {expected} at `{path}`")]
    ShapeMismatch {
        path: String,
        expected: &'static str,
    },
    #[error("field `{field}` is missing at `{path}`")]
    RequiredFieldMissing { path: String, field: String },
    #[error("field `{field}` is undeclared at `{path}`")]
    UndeclaredField { path: String, field: String },
    #[error("presence contract names undeclared field `{field}` at `{path}`")]
    PresenceFieldUndeclared { path: String, field: String },
    #[error("duplicate set element at `{path}`")]
    DuplicateSetElement { path: String },
    #[error("value does not satisfy its declared contract at `{path}`")]
    ContractMismatch { path: String },
    #[error("refinement recursion exceeded the supported limit")]
    RefinementDepthExceeded,
    #[error("value violates the declared domain at `{path}`")]
    DomainViolation { path: String },
    #[error("value has an invalid temporal encoding at `{path}`")]
    InvalidTemporalEncoding { path: String },
    #[error("value contract intersection recursion exceeded the supported limit")]
    IntersectionDepthExceeded,
    #[error("value contract intersection contains distinct domain identities")]
    ConflictingValueDomains,
    #[error("value contract intersection contains incompatible semantic string types")]
    ConflictingSemanticStringTypes,
    #[error("value contract intersection contains incompatible temporal encodings")]
    ConflictingTemporalEncodings,
    #[error("value contract intersection has no representable specialized carrier")]
    UnrepresentableSpecializedIntersection,
    #[error("value-domain recursion exceeded the supported materialization limit")]
    DomainResolutionDepthExceeded,
    #[error("value domain is absent from the catalog")]
    DomainNotFound,
    #[error("array value domain has no element contract")]
    ArrayDomainElementMissing,
    #[error("value contracts have no compatible intersection")]
    IncompatibleIntersection,
    #[error("value contracts have no compatible refinement")]
    IncompatibleRefinement,
    #[error("native money value is required at `{path}`")]
    MoneyRequired { path: String },
    #[error("money currency differs from its declared domain at `{path}`")]
    MoneyCurrencyMismatch { path: String },
    #[error("value is not valid JSON data at `{path}`")]
    InvalidJson { path: String },
    #[error("value does not satisfy its materialized contract at `{path}`")]
    MaterializedValueMismatch { path: String },
    #[error("invalid temporal value")]
    Temporal(#[from] crate::value_order::TemporalValueError),
    #[error(transparent)]
    TemporalCodec(#[from] crate::temporal_value::TemporalValueError),
    #[error(transparent)]
    Equality(#[from] crate::value_equality::ValueEqualityError),
    #[error("value domain validation failed at `{path}`: {source}")]
    Domain {
        path: String,
        #[source]
        source: crate::ValueDomainViolation,
    },
    #[error("quantification requires a non-null array")]
    QuantificationRequiresNonNullArray,
    #[error("quantification requires an array")]
    QuantificationRequiresArray,
    #[error("quantified predicate requires a non-null Boolean")]
    QuantifiedPredicateRequiresBoolean,
    #[error("unresolved symbol has no materialized type")]
    UnresolvedSymbol,
    #[error("value inference references a source field that is absent")]
    SourceFieldMissing,
    #[error("value inference source node is absent")]
    SourceNodeMissing,
    #[error("value inference source is excluded by its declared projection")]
    SourceFieldExcluded,
    #[error("value inference source has incompatible field contracts")]
    IncompatibleSourceFieldContracts,
    #[error("value inference source operation is unsupported")]
    UnsupportedSourceOperation,
    #[error("unevaluated expression is not a materialized literal")]
    UnevaluatedExpressionNotLiteral,
    #[error("length requires a string, array or record")]
    InvalidLengthOperand,
    #[error(transparent)]
    ValueExpression(#[from] Box<crate::value_expression::InferenceError>),
    #[error(transparent)]
    Reference(#[from] crate::entity_ref_value::EntityRefValueError),
}

impl From<crate::value_expression::InferenceError> for ValueContractError {
    fn from(error: crate::value_expression::InferenceError) -> Self {
        Self::ValueExpression(Box::new(error))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum ValueShape {
    /// A Python mapping view of a closed, typed record; no entity authority.
    MappingRecord {
        record: Box<ValueContract>,
    },
    /// Unordered, unique Python values; array is only the wire encoding.
    Set {
        element: Box<ValueContract>,
    },
    Temporal {
        kind: crate::temporal_value::TemporalKind,
        wire: Option<crate::TemporalWireFormat>,
    },
    Scalar {
        field_type: FieldType,
    },
    Array {
        element: Box<ValueContract>,
    },
    /// Python dictionary data. Keys are strings at the materialization boundary;
    /// no particular key is guaranteed present and no receiver authority is implied.
    Dictionary {
        key: Box<ValueContract>,
        value: Box<ValueContract>,
    },
    Record {
        fields: BTreeMap<String, ValueContract>,
    },
    /// A typed observation: listed fields retain their types when present.
    /// Presence is independent of nullable, and undeclared fields are not values.
    ObservedRecord {
        fields: BTreeMap<String, ValueContract>,
        optional_fields: std::collections::BTreeSet<String>,
    },
    Null,
    /// No values inhabit an empty literal's element type.
    Never,
    Union {
        variants: Vec<ValueContract>,
    },
}

#[derive(Clone, Copy)]
enum ValidationBoundary {
    Materialized,
    /// Validate known observations without closing a graph row's field set.
    Observation,
}

impl ValueContract {
    /// Whether every inhabitant is a record, suitable for direct row storage.
    pub fn is_non_null_record(&self) -> bool {
        !self.nullable
            && matches!(
                self.shape,
                ValueShape::Record { .. } | ValueShape::ObservedRecord { .. }
            )
    }

    pub fn record(
        fields: BTreeMap<String, Self>,
        optional_fields: std::collections::BTreeSet<String>,
    ) -> Self {
        Self {
            shape: if optional_fields.is_empty() {
                ValueShape::Record { fields }
            } else {
                ValueShape::ObservedRecord {
                    fields,
                    optional_fields,
                }
            },
            domain: None,
            nullable: false,
        }
    }

    /// Project an observation onto its declared value columns. Absence is preserved.
    /// Closed records and scalar values are not weakened or coerced.
    pub fn observed_value(
        &self,
        value: &crate::Value,
        cgs: &CGS,
        entry: &str,
    ) -> Result<crate::Value, ValueContractError> {
        self.observed_value_in(value, cgs, entry, &|_| None)
    }

    /// Resolve each pinned domain against its own catalog, without merging catalog graphs.
    pub fn observed_value_in<'a>(
        &self,
        value: &crate::Value,
        cgs: &'a CGS,
        entry: &str,
        catalogs: &dyn Fn(&str) -> Option<&'a CGS>,
    ) -> Result<crate::Value, ValueContractError> {
        self.observed_value_at(value, cgs, entry, 0, catalogs)
    }

    fn observed_value_at<'a>(
        &self,
        value: &crate::Value,
        cgs: &'a CGS,
        entry: &str,
        depth: usize,
        catalogs: &dyn Fn(&str) -> Option<&'a CGS>,
    ) -> Result<crate::Value, ValueContractError> {
        if depth >= 64 {
            return Err(ValueContractError::ObservationDepthExceeded);
        }

        match (&self.shape, value) {
            (ValueShape::MappingRecord { record }, _) => {
                record.observed_value_at(value, cgs, entry, depth + 1, catalogs)
            }
            (
                ValueShape::Scalar {
                    field_type: FieldType::EntityRef { entry_id, target },
                },
                crate::Value::Object(values),
            ) if values.contains_key("_ref") => {
                let owner = if entry_id.as_str().is_empty() || entry_id.as_str() == entry {
                    cgs
                } else {
                    catalogs(entry_id.as_str()).ok_or_else(|| {
                        ValueContractError::ReferenceCatalogNotLoaded {
                            entry_id: entry_id.to_string(),
                        }
                    })?
                };
                let entity = owner.get_entity(target.as_str()).ok_or_else(|| {
                    ValueContractError::ReferenceTargetMissing {
                        target: target.to_string(),
                    }
                })?;
                Ok(crate::entity_ref_value::observed_reference_payload(
                    value,
                    entity,
                    target.as_str(),
                )?)
            }
            (ValueShape::ObservedRecord { fields, .. }, crate::Value::Object(values)) => fields
                .iter()
                .filter_map(|(name, contract)| {
                    values.get(name).map(|value| {
                        contract
                            .observed_value_at(value, cgs, entry, depth + 1, catalogs)
                            .map(|value| (name.clone(), value))
                    })
                })
                .collect::<Result<indexmap::IndexMap<_, _>, _>>()
                .map(crate::Value::Object),
            (ValueShape::Dictionary { value: element, .. }, crate::Value::Object(values)) => values
                .iter()
                .map(|(key, value)| {
                    element
                        .observed_value_at(value, cgs, entry, depth + 1, catalogs)
                        .map(|v| (key.clone(), v))
                })
                .collect::<Result<indexmap::IndexMap<_, _>, _>>()
                .map(crate::Value::Object),
            (
                ValueShape::Array { element } | ValueShape::Set { element },
                crate::Value::Array(values),
            ) => values
                .iter()
                .map(|value| element.observed_value_at(value, cgs, entry, depth + 1, catalogs))
                .collect::<Result<Vec<_>, _>>()
                .map(crate::Value::Array),
            (ValueShape::Record { fields }, crate::Value::Object(values)) => values
                .iter()
                .map(|(name, value)| {
                    fields
                        .get(name)
                        .map_or_else(
                            || Ok(value.clone()),
                            |contract| {
                                contract.observed_value_at(value, cgs, entry, depth + 1, catalogs)
                            },
                        )
                        .map(|value| (name.clone(), value))
                })
                .collect::<Result<indexmap::IndexMap<_, _>, _>>()
                .map(crate::Value::Object),
            (ValueShape::Union { variants }, _) => {
                let mut candidates = variants.iter().filter_map(|variant| {
                    let projected = variant
                        .observed_value_at(value, cgs, entry, depth + 1, catalogs)
                        .ok()?;
                    variant
                        .validate_in(&projected, cgs, entry, "observation", catalogs)
                        .ok()?;
                    Some(projected)
                });
                let result = candidates
                    .next()
                    .ok_or(ValueContractError::NoUnionVariantMatches)?;
                if candidates.any(|candidate| candidate != result) {
                    return Err(ValueContractError::AmbiguousUnionProjection);
                }
                Ok(result)
            }
            _ => Ok(value.clone()),
        }
    }

    pub fn data_value<E>(
        value: &crate::PlasmDataValue,
        resolve: &mut (impl FnMut(&str, &[String]) -> Result<ValueContract, E> + ?Sized),
    ) -> Result<Self, E>
    where
        E: From<ValueContractError> + From<crate::value_expression::InferenceError>,
    {
        use crate::PlasmDataValue as V;
        Ok(match value {
            V::Quantified {
                collection,
                binding,
                predicate,
                ..
            } => {
                let collection = Self::data_value(collection, resolve)?;
                if collection.nullable {
                    return Err(ValueContractError::QuantificationRequiresNonNullArray.into());
                }
                let ValueShape::Array { element } = collection.shape else {
                    return Err(ValueContractError::QuantificationRequiresArray.into());
                };
                if matches!(element.shape, ValueShape::Never) {
                    return Ok(Self::scalar(FieldType::Boolean));
                }
                let mut scoped_resolve = |name: &str, path: &[String]| {
                    if name != binding {
                        return resolve(name, path);
                    }
                    let mut value = *element.clone();
                    for field in path {
                        value = value.field(field)?;
                    }
                    Ok(value)
                };
                // Erase the recursive scope wrapper to keep monomorphization bounded.
                let result = Self::data_value(
                    predicate,
                    &mut scoped_resolve as &mut dyn FnMut(&str, &[String]) -> Result<Self, E>,
                )?;
                if result.nullable || result.summary() != crate::SyntheticValueKind::Boolean {
                    return Err(ValueContractError::QuantifiedPredicateRequiresBoolean.into());
                }
                Self::scalar(FieldType::Boolean)
            }
            V::Expression { expression } => expression.infer(|v| Self::data_value(v, resolve))?,
            V::BindingSymbol { binding, path } => resolve(binding, path)?,
            V::NodeSymbol { node, path, .. } => resolve(node, path)?,
            V::Literal { value } => Self::literal(value.value())?,
            V::Array { items } => Self::array(
                items
                    .iter()
                    .map(|v| Self::data_value(v, resolve))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            V::Object { fields } => Self {
                shape: ValueShape::Record {
                    fields: fields
                        .iter()
                        .map(|(k, v)| Ok((k.clone(), Self::data_value(v, resolve)?)))
                        .collect::<Result<_, E>>()?,
                },
                domain: None,
                nullable: false,
            },
            V::Template { .. } => Self::scalar(FieldType::String),
            V::EntityRefKey { api, entity, .. } => Self::scalar(FieldType::EntityRef {
                entry_id: api.clone().into(),
                target: entity.clone().into(),
            }),
            V::Symbol { .. } => return Err(ValueContractError::UnresolvedSymbol.into()),
        })
    }

    fn array(elements: Vec<Self>) -> Self {
        let mut variants = Vec::new();
        for element in elements {
            if !variants.contains(&element) {
                variants.push(element);
            }
        }
        let element = if variants.len() == 1 {
            variants.remove(0)
        } else {
            Self {
                shape: if variants.is_empty() {
                    ValueShape::Never
                } else {
                    ValueShape::Union { variants }
                },
                domain: None,
                nullable: false,
            }
        };
        Self {
            shape: ValueShape::Array {
                element: Box::new(element),
            },
            domain: None,
            nullable: false,
        }
    }

    pub fn literal(value: &crate::Value) -> Result<Self, ValueContractError> {
        use crate::Value as V;
        Ok(match value {
            V::Null => Self {
                shape: ValueShape::Null,
                domain: None,
                nullable: true,
            },
            V::Bool(_) => Self::scalar(FieldType::Boolean),
            V::Integer(_) | V::Unsigned(_) => Self::scalar(FieldType::Integer),
            V::Float(_) => Self::scalar(FieldType::Number),
            V::String(_) => Self::scalar(FieldType::String),
            V::Money(_) => Self::scalar(FieldType::Money),
            V::Array(items) => {
                Self::array(items.iter().map(Self::literal).collect::<Result<_, _>>()?)
            }
            V::Object(fields) => Self {
                shape: ValueShape::Record {
                    fields: fields
                        .iter()
                        .map(|(k, v)| Ok((k.clone(), Self::literal(v)?)))
                        .collect::<Result<_, ValueContractError>>()?,
                },
                domain: None,
                nullable: false,
            },
            _ => return Err(ValueContractError::UnevaluatedExpressionNotLiteral),
        })
    }
    pub fn aggregate(
        function: crate::AggregateFunction,
        input: Option<&Self>,
    ) -> Result<Self, crate::row_plan::contracts::RowContractError> {
        crate::row_plan::contracts::reduction_contract(function, input)
    }
    /// Infer row expressions without executing them. Aliases retain their domain;
    /// computed scalars use the result type rather than impersonating an input domain.
    pub fn with_expr<E>(
        expr: &crate::WithExpr,
        field: &mut impl FnMut(&crate::FieldPath) -> Result<Self, E>,
    ) -> Result<Self, E>
    where
        E: From<crate::row_plan::contracts::RowContractError>
            + From<crate::value_arithmetic::ArithmeticContractError>,
    {
        use crate::{WithExpr as E, WithLiteral as L};
        Ok(match expr {
            E::Field(path) => field(path)?,
            E::Literal(literal) => match literal {
                L::Null => Self {
                    shape: ValueShape::Null,
                    domain: None,
                    nullable: true,
                },
                L::Bool(_) => Self::scalar(FieldType::Boolean),
                L::Integer(_) => Self::scalar(FieldType::Integer),
                L::Number(_) => Self::scalar(FieldType::Number),
                L::String(_) => Self::scalar(FieldType::String),
            },
            E::Len { field: path } => {
                let input = field(path)?;
                if !matches!(
                    input.summary(),
                    crate::SyntheticValueKind::String
                        | crate::SyntheticValueKind::Array
                        | crate::SyntheticValueKind::Object
                ) {
                    return Err(
                        crate::row_plan::contracts::RowContractError::InvalidLengthOperand.into(),
                    );
                }
                let mut result = Self::scalar(FieldType::Integer);
                result.nullable = input.nullable;
                result
            }
            E::Now => crate::temporal_value::TemporalKind::Datetime.contract(),
            E::Arith { op, lhs, rhs } => {
                let left = Self::with_expr(lhs, field)?;
                let right = Self::with_expr(rhs, field)?;
                Self::arithmetic(*op, &left, &right)?
            }
            E::When {
                lhs,
                rhs,
                then,
                else_,
                ..
            } => {
                // Validate condition dependencies as well as both possible results.
                Self::with_expr(lhs, field)?;
                Self::with_expr(rhs, field)?;
                Self::join(
                    Self::with_expr(then, field)?,
                    Self::with_expr(else_, field)?,
                )
            }
        })
    }
    pub fn summary(&self) -> crate::SyntheticValueKind {
        use crate::SyntheticValueKind as K;
        match &self.shape {
            ValueShape::Temporal { .. } => K::Temporal,
            ValueShape::Never | ValueShape::Union { .. } => K::Unknown,
            ValueShape::Null => K::Null,
            ValueShape::Array { .. } | ValueShape::Set { .. } => K::Array,
            ValueShape::MappingRecord { .. } => K::Object,
            ValueShape::Record { .. }
            | ValueShape::ObservedRecord { .. }
            | ValueShape::Dictionary { .. } => K::Object,
            ValueShape::Scalar { field_type } => match field_type {
                FieldType::Boolean => K::Boolean,
                FieldType::Integer => K::Integer,
                FieldType::Number => K::Number,
                FieldType::Money => K::Money,
                FieldType::EntityRef { .. } => K::EntityRef,
                FieldType::Date => K::Temporal,
                FieldType::Array | FieldType::MultiSelect => K::Array,
                FieldType::Json | FieldType::Blob => K::Object,
                _ => K::String,
            },
        }
    }
    pub fn scalar(field_type: FieldType) -> Self {
        if field_type == FieldType::Date {
            return crate::temporal_value::TemporalKind::Datetime.contract();
        }
        Self {
            shape: ValueShape::Scalar { field_type },
            domain: None,
            nullable: false,
        }
    }

    pub fn from_domain(
        cgs: &CGS,
        entry: &str,
        key: &ValueDomainKey,
    ) -> Result<Self, ValueContractError> {
        Self::resolve(cgs, entry, key, &mut Vec::new())
    }

    fn resolve(
        cgs: &CGS,
        entry: &str,
        key: &ValueDomainKey,
        stack: &mut Vec<ValueDomainKey>,
    ) -> Result<Self, ValueContractError> {
        if stack.len() >= 64 || stack.contains(key) {
            return Err(ValueContractError::DomainResolutionDepthExceeded);
        }
        let value = cgs
            .values
            .get(key.as_str())
            .ok_or(ValueContractError::DomainNotFound)?;
        stack.push(key.clone());
        let shape = if value.field_type == FieldType::Array {
            let item = value
                .array_items
                .as_ref()
                .ok_or(ValueContractError::ArrayDomainElementMissing)?;
            ValueShape::Array {
                element: Box::new(Self::resolve(cgs, entry, item.kind.registry_key(), stack)?),
            }
        } else if value.field_type == FieldType::Date {
            let wire = match value.domain.to_value_format() {
                Some(crate::ValueWireFormat::Temporal(wire)) => Some(wire),
                _ => None,
            };
            ValueShape::Temporal {
                kind: if wire == Some(crate::TemporalWireFormat::Iso8601Date) {
                    crate::temporal_value::TemporalKind::Date
                } else {
                    crate::temporal_value::TemporalKind::Datetime
                },
                wire,
            }
        } else {
            ValueShape::Scalar {
                field_type: value.field_type.clone(),
            }
        };
        stack.pop();
        Ok(Self {
            shape,
            domain: Some(DomainRef {
                entry_id: entry.into(),
                catalog_hash: cgs.catalog_cgs_hash_hex(),
                value_ref: key.clone(),
            }),
            nullable: false,
        })
    }

    /// Validate against the pinned registry; no coercion, stringification or numeric widening.
    pub fn validate(
        &self,
        value: &crate::Value,
        cgs: &CGS,
        entry: &str,
        path: &str,
    ) -> Result<(), ValueContractError> {
        self.validate_in(value, cgs, entry, path, &|_| None)
    }

    /// Validate recursive domains using the owning catalog of each value.
    pub fn validate_in<'a>(
        &self,
        value: &crate::Value,
        cgs: &'a CGS,
        entry: &str,
        path: &str,
        catalogs: &dyn Fn(&str) -> Option<&'a CGS>,
    ) -> Result<(), ValueContractError> {
        self.validate_at(
            value,
            cgs,
            entry,
            path,
            0,
            catalogs,
            ValidationBoundary::Materialized,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn validate_at<'a>(
        &self,
        value: &crate::Value,
        cgs: &'a CGS,
        entry: &str,
        path: &str,
        depth: usize,
        catalogs: &dyn Fn(&str) -> Option<&'a CGS>,
        boundary: ValidationBoundary,
    ) -> Result<(), ValueContractError> {
        if depth >= 64 {
            return Err(ValueContractError::ValidationDepthExceeded);
        }
        let (cgs, entry) = match &self.domain {
            Some(reference) if reference.entry_id != entry => (
                catalogs(&reference.entry_id).ok_or_else(|| {
                    ValueContractError::DomainCatalogNotLoaded {
                        entry_id: reference.entry_id.clone(),
                    }
                })?,
                reference.entry_id.as_str(),
            ),
            _ => (cgs, entry),
        };
        let domain = if let Some(reference) = &self.domain {
            if reference.entry_id != entry || reference.catalog_hash != cgs.catalog_cgs_hash_hex() {
                return Err(ValueContractError::DomainCatalogPinMismatch {
                    entry_id: reference.entry_id.clone(),
                });
            }
            Some(
                &cgs.values
                    .get(reference.value_ref.as_str())
                    .ok_or_else(|| ValueContractError::DomainMissing {
                        entry_id: reference.entry_id.clone(),
                        value_ref: reference.value_ref.to_string(),
                    })?
                    .domain,
            )
        } else {
            None
        };
        if value.is_null() && self.nullable {
            return Ok(());
        }
        let valid = match &self.shape {
            ValueShape::MappingRecord { record } => {
                record.validate_at(value, cgs, entry, path, depth + 1, catalogs, boundary)?;
                true
            }
            ValueShape::Temporal { kind, wire } => {
                crate::temporal_value::components(value, *kind, *wire)?;
                true
            }
            ValueShape::Never => false,
            ValueShape::Union { variants } => variants.iter().any(|t| {
                t.validate_at(value, cgs, entry, path, depth + 1, catalogs, boundary)
                    .is_ok()
            }),
            ValueShape::Null => value.is_null(),
            ValueShape::Array { element } | ValueShape::Set { element } => {
                let values = value
                    .as_array()
                    .ok_or_else(|| ValueContractError::ShapeMismatch {
                        path: path.into(),
                        expected: "array",
                    })?;
                if matches!(self.shape, ValueShape::Set { .. }) {
                    use crate::value_equality::Equatable;
                    let equality = element.equality()?;
                    let mut keys = Vec::with_capacity(values.len());
                    for value in values {
                        let key = equality.key(value)?;
                        if keys.contains(&key) {
                            return Err(ValueContractError::DuplicateSetElement {
                                path: path.into(),
                            });
                        }
                        keys.push(key);
                    }
                }
                for (i, v) in values.iter().enumerate() {
                    element.validate_at(
                        v,
                        cgs,
                        entry,
                        &format!("{path}[{i}]"),
                        depth + 1,
                        catalogs,
                        boundary,
                    )?;
                }
                true
            }
            ValueShape::Dictionary {
                key,
                value: element,
            } => {
                let values =
                    value
                        .as_object()
                        .ok_or_else(|| ValueContractError::ShapeMismatch {
                            path: path.into(),
                            expected: "dictionary",
                        })?;
                for (name, value) in values {
                    key.validate_at(
                        &crate::Value::String(name.clone()),
                        cgs,
                        entry,
                        path,
                        depth + 1,
                        catalogs,
                        boundary,
                    )?;
                    element.validate_at(
                        value,
                        cgs,
                        entry,
                        &format!("{path}[{name:?}]"),
                        depth + 1,
                        catalogs,
                        boundary,
                    )?;
                }
                true
            }
            ValueShape::ObservedRecord {
                fields,
                optional_fields,
            } => {
                if !optional_fields.iter().all(|name| fields.contains_key(name)) {
                    let field = optional_fields
                        .iter()
                        .find(|name| !fields.contains_key(*name))
                        .cloned()
                        .unwrap_or_default();
                    return Err(ValueContractError::PresenceFieldUndeclared {
                        path: path.into(),
                        field,
                    });
                }
                let values =
                    value
                        .as_object()
                        .ok_or_else(|| ValueContractError::ShapeMismatch {
                            path: path.into(),
                            expected: "observed record",
                        })?;
                for (name, field) in fields {
                    match values.get(name) {
                        Some(value) => field.validate_at(
                            value,
                            cgs,
                            entry,
                            &format!("{path}.{name}"),
                            depth + 1,
                            catalogs,
                            boundary,
                        )?,
                        None if optional_fields.contains(name) => {}
                        None => {
                            return Err(ValueContractError::RequiredFieldMissing {
                                path: path.into(),
                                field: name.clone(),
                            })
                        }
                    }
                }
                if let Some(name) = values
                    .keys()
                    .find(|name| !fields.contains_key(*name))
                    .filter(|_| matches!(boundary, ValidationBoundary::Materialized))
                {
                    return Err(ValueContractError::UndeclaredField {
                        path: path.into(),
                        field: name.clone(),
                    });
                }
                true
            }
            ValueShape::Record { fields } => {
                let values =
                    value
                        .as_object()
                        .ok_or_else(|| ValueContractError::ShapeMismatch {
                            path: path.into(),
                            expected: "record",
                        })?;
                for (name, field) in fields {
                    let v = values.get(name).ok_or_else(|| {
                        ValueContractError::RequiredFieldMissing {
                            path: path.into(),
                            field: name.clone(),
                        }
                    })?;
                    field.validate_at(
                        v,
                        cgs,
                        entry,
                        &format!("{path}.{name}"),
                        depth + 1,
                        catalogs,
                        boundary,
                    )?;
                }
                values.len() == fields.len()
            }
            ValueShape::Scalar { field_type } => match field_type {
                FieldType::Boolean => value.as_bool().is_some(),
                FieldType::Integer => value.as_integer().is_some(),
                FieldType::Number => value.as_number().is_some_and(f64::is_finite),
                FieldType::String | FieldType::Select | FieldType::Uuid | FieldType::DigitId => {
                    value.is_string()
                }
                FieldType::Date => value.is_string() || value.as_integer().is_some(),
                FieldType::MultiSelect => {
                    let items =
                        value
                            .as_array()
                            .ok_or_else(|| ValueContractError::ShapeMismatch {
                                path: path.into(),
                                expected: "enum array",
                            })?;
                    for (i, item) in items.iter().enumerate() {
                        let text =
                            item.as_str()
                                .ok_or_else(|| ValueContractError::ShapeMismatch {
                                    path: format!("{path}[{i}]"),
                                    expected: "enum string",
                                })?;
                        if let Some(d) = domain {
                            d.validate_string_value(text).map_err(|error| {
                                ValueContractError::Domain {
                                    path: format!("{path}[{i}]"),
                                    source: error.contract_violation(),
                                }
                            })?;
                            if !d
                                .enum_tokens()
                                .is_some_and(|tokens| tokens.iter().any(|t| t == text))
                            {
                                return Err(ValueContractError::DomainViolation {
                                    path: format!("{path}[{i}]"),
                                });
                            }
                        }
                    }
                    true
                }
                FieldType::Money => {
                    let crate::Value::Money(money) = value else {
                        return Err(ValueContractError::MoneyRequired { path: path.into() });
                    };
                    if let Some(currency) = domain.and_then(|d| d.currency.as_deref()) {
                        if money.currency() != Some(currency) {
                            return Err(ValueContractError::MoneyCurrencyMismatch {
                                path: path.into(),
                            });
                        }
                    }
                    true
                }
                FieldType::EntityRef { .. } => {
                    crate::entity_ref_value::EntityRefPayload::try_from_value(value).is_ok()
                }
                FieldType::Json | FieldType::Blob => match validate_json(value, depth + 1) {
                    Ok(()) => true,
                    Err(()) => return Err(ValueContractError::InvalidJson { path: path.into() }),
                },
                FieldType::Array => false, // An array always requires its element contract.
            },
        };
        if !valid {
            return Err(ValueContractError::MaterializedValueMismatch { path: path.into() });
        }
        let encoded = if domain.is_some() && value.get("__plasm_temporal").is_some() {
            if let ValueShape::Temporal { kind, wire } = &self.shape {
                Some(crate::temporal_value::encode(
                    value,
                    wire.unwrap_or(if *kind == crate::temporal_value::TemporalKind::Date {
                        crate::TemporalWireFormat::Iso8601Date
                    } else {
                        crate::TemporalWireFormat::Rfc3339
                    }),
                )?)
            } else {
                None
            }
        } else {
            None
        };
        let value = encoded.as_ref().unwrap_or(value);
        if let Some(domain) = domain {
            if let Some(items) = value.as_array() {
                if domain
                    .constraints
                    .min_length
                    .is_some_and(|n| items.len() < n)
                    || domain
                        .constraints
                        .max_length
                        .is_some_and(|n| items.len() > n)
                {
                    return Err(ValueContractError::DomainViolation { path: path.into() });
                }
            }
            if let Some(text) = value.as_str() {
                domain
                    .validate_string_value(text)
                    .map_err(|error| ValueContractError::Domain {
                        path: path.to_owned(),
                        source: error.contract_violation(),
                    })?;
                use crate::value_domain::ProfileId;
                match domain.profile {
                    Some(ProfileId::Rfc3339) => {
                        chrono::DateTime::parse_from_rfc3339(text).map_err(|_| {
                            ValueContractError::InvalidTemporalEncoding { path: path.into() }
                        })?;
                    }
                    Some(ProfileId::Iso8601NaiveDatetime) => {
                        crate::temporal_value::components(
                            value,
                            crate::temporal_value::TemporalKind::Datetime,
                            Some(crate::TemporalWireFormat::Iso8601NaiveDatetime),
                        )?;
                    }
                    Some(ProfileId::Iso8601Date) => {
                        chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").map_err(|_| {
                            ValueContractError::InvalidTemporalEncoding { path: path.into() }
                        })?;
                    }
                    _ => {}
                }
            } else if let Some(number) = value.as_number() {
                domain.validate_number_value(number).map_err(|error| {
                    ValueContractError::Domain {
                        path: path.to_owned(),
                        source: error.contract_violation(),
                    }
                })?;
            }
        }
        Ok(())
    }

    pub fn python_type(&self) -> String {
        let shape = match &self.shape {
            ValueShape::Temporal { kind, .. } => kind.python_name().into(),
            ValueShape::Never => "Never".into(),
            ValueShape::Union { variants } => variants
                .iter()
                .map(Self::python_type)
                .collect::<Vec<_>>()
                .join(" | "),
            ValueShape::Null => "None".into(),
            ValueShape::Array { element } => format!("list[{}]", element.python_type()),
            ValueShape::Set { element } => format!("set[{}]", element.python_type()),
            ValueShape::MappingRecord { record } => record.python_type(),
            ValueShape::Dictionary { key, value } => {
                format!("dict[{}, {}]", key.python_type(), value.python_type())
            }
            ValueShape::Record { .. } | ValueShape::ObservedRecord { .. } => "Record".into(),
            ValueShape::Scalar { field_type } => match field_type {
                FieldType::Boolean => "bool",
                FieldType::Integer => "int",
                FieldType::Number => "float",
                FieldType::MultiSelect => "list[str]",
                FieldType::Money => "Money",
                FieldType::EntityRef { .. } => "EntityRef",
                FieldType::Json => "JsonValue",
                FieldType::Blob => "Blob",
                FieldType::Date => "datetime",
                FieldType::Array => "list",
                _ => "str",
            }
            .into(),
        };
        if self.nullable {
            format!("{shape} | None")
        } else {
            shape
        }
    }
}

fn validate_json(value: &crate::Value, depth: usize) -> Result<(), ()> {
    if depth >= 64 {
        return Err(());
    }
    match value {
        crate::Value::Array(values) => {
            for v in values {
                validate_json(v, depth + 1)?;
            }
        }
        crate::Value::Object(values) => {
            for v in values.values() {
                validate_json(v, depth + 1)?;
            }
        }
        crate::Value::Null
        | crate::Value::Bool(_)
        | crate::Value::String(_)
        | crate::Value::Integer(_)
        | crate::Value::Unsigned(_) => {}
        crate::Value::Float(v) if v.is_finite() => {}
        _ => return Err(()),
    }
    Ok(())
}

impl ValueContract {
    /// Structural value access, including every variant of a union. Presence
    /// remains a runtime obligation; selecting a field creates no authority.
    pub fn field(&self, name: &str) -> Result<Self, ValueContractError> {
        match &self.shape {
            ValueShape::Record { fields } | ValueShape::ObservedRecord { fields, .. } => fields
                .get(name)
                .cloned()
                .ok_or_else(|| ValueContractError::FieldMissing {
                    name: name.to_owned(),
                }),
            ValueShape::Union { variants } => variants
                .iter()
                .map(|v| v.field(name))
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .reduce(Self::join)
                .ok_or(ValueContractError::EmptyUnion),
            _ => Err(ValueContractError::FieldRequiresRecord {
                name: name.to_owned(),
            }),
        }
    }
}

#[cfg(test)]
mod presence_tests {
    use super::*;
    use crate::fixture_value as json;
    use std::collections::BTreeSet;

    #[test]
    fn domain_fault_retains_path_and_semantic_cause_without_observed_value() {
        let cgs = crate::loader::load_schema_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/plasm_language_matrix"),
        )
        .unwrap();
        let key = ValueDomainKey::new("nv_lang_item_code").unwrap();
        let contract = ValueContract::from_domain(&cgs, "matrix", &key).unwrap();
        let error = contract
            .validate(&json!("x"), &cgs, "matrix", "rows[2].code")
            .unwrap_err();
        assert!(
            matches!(&error, ValueContractError::Domain { path, source: crate::ValueDomainViolation::BelowMinLength(3) } if path == "rows[2].code")
        );
        let cause = std::error::Error::source(&error).unwrap();
        assert!(matches!(
            cause.downcast_ref::<crate::ValueDomainViolation>(),
            Some(crate::ValueDomainViolation::BelowMinLength(3))
        ));
        assert!(!error.to_string().contains("\"x\""));
    }

    #[test]
    fn field_errors_distinguish_missing_field_from_non_record_shape() {
        let record = ValueContract::record(BTreeMap::new(), Default::default());
        assert_eq!(
            record.field("absent"),
            Err(ValueContractError::FieldMissing {
                name: "absent".into()
            })
        );
        assert_eq!(
            ValueContract::scalar(FieldType::String).field("name"),
            Err(ValueContractError::FieldRequiresRecord {
                name: "name".into()
            })
        );
    }

    #[test]
    fn reduction_contracts_preserve_native_numeric_result_kinds() {
        use crate::{AggregateFunction as A, SyntheticValueKind as K};
        for (input, expected_sum, expected_avg) in [
            (FieldType::Integer, K::Integer, K::Number),
            (FieldType::Number, K::Number, K::Number),
            (FieldType::Money, K::Money, K::Money),
        ] {
            let input = ValueContract::scalar(input);
            assert_eq!(
                ValueContract::aggregate(A::Sum, Some(&input))
                    .unwrap()
                    .summary(),
                expected_sum
            );
            assert_eq!(
                ValueContract::aggregate(A::Avg, Some(&input))
                    .unwrap()
                    .summary(),
                expected_avg
            );
            for function in [A::Min, A::Max, A::First, A::Last] {
                let output = ValueContract::aggregate(function, Some(&input)).unwrap();
                assert_eq!(output.summary(), input.summary());
                assert!(output.nullable);
            }
        }
    }

    #[test]
    fn observed_references_use_pinned_structural_identity_not_embedded_fields() {
        let cgs = crate::loader::load_schema_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/plasm_language_matrix"),
        )
        .unwrap();
        let contract = |target: &str| {
            ValueContract::scalar(FieldType::EntityRef {
                entry_id: "matrix".into(),
                target: target.into(),
            })
        };
        let simple = contract("LangLine");
        let row = json!({"_ref":{"kind":"simple","entity":"LangLine","id":"l1"}, "optional":null, "children":[], "id":"untrusted-field"});
        assert_eq!(
            simple.observed_value(&row, &cgs, "matrix").unwrap(),
            json!("l1")
        );
        assert!(simple.observed_value(&row, &cgs, "other").is_err());
        for bad in [
            json!({"_ref":{"kind":"simple","entity":"LangItem","id":"l1"}}),
            json!({"_ref":{"kind":"simple","entity":"LangLine","id":7}}),
            json!({"_ref":"LangLine:l1"}),
        ] {
            assert!(simple.observed_value(&bad, &cgs, "matrix").is_err());
        }
        assert!(simple
            .validate(&json!({"arbitrary":null}), &cgs, "matrix", "ref")
            .is_err());
        let compound = contract("CompoundBranch");
        let parts = json!({"owner":"o","item_id":"i","name":"n"});
        let row = json!({"_ref":{"kind":"compound","entity":"CompoundBranch","parts":{"owner":"o","item_id":"i","name":"n"}},"children":[],"optional":null});
        assert_eq!(
            compound.observed_value(&row, &cgs, "matrix").unwrap(),
            parts
        );
        let bad =
            json!({"_ref":{"kind":"compound","entity":"CompoundBranch","parts":{"name":"n"}}});
        assert!(compound.observed_value(&bad, &cgs, "matrix").is_err());
        assert!(simple.observed_value(&row, &cgs, "matrix").is_err());
    }

    #[test]
    fn observed_presence_is_independent_of_nullability_and_survives_serde() {
        let cgs = crate::loader::load_schema_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/python_value_contract"),
        )
        .unwrap();
        let mut nullable = ValueContract::scalar(FieldType::String);
        nullable.nullable = true;
        let contract = ValueContract::record(
            BTreeMap::from([
                ("id".into(), ValueContract::scalar(FieldType::Integer)),
                ("label".into(), nullable),
            ]),
            BTreeSet::from(["label".into()]),
        );
        let restored: ValueContract =
            serde_json::from_value(serde_json::to_value(&contract).unwrap()).unwrap();
        assert_eq!(contract, restored);
        for value in [
            json!({"id":1}),
            json!({"id":1,"label":null}),
            json!({"id":1,"label":"ok"}),
        ] {
            contract.validate(&value, &cgs, "types", "row").unwrap();
            assert_eq!(
                contract.observed_value(&value, &cgs, "types").unwrap(),
                value
            );
        }
        for value in [
            json!({}),
            json!({"id":null}),
            json!({"id":1,"label":3}),
            json!({"id":1,"extra":true}),
        ] {
            assert!(contract.validate(&value, &cgs, "types", "row").is_err());
        }
        let union = ValueContract {
            shape: ValueShape::Union {
                variants: vec![contract.clone(), ValueContract::scalar(FieldType::Integer)],
            },
            domain: None,
            nullable: false,
        };
        assert_eq!(
            union
                .observed_value(&json!({"id":1,"_ref":"metadata"}), &cgs, "types")
                .unwrap(),
            json!({"id":1})
        );
        assert_eq!(
            union.observed_value(&json!(7), &cgs, "types").unwrap(),
            json!(7)
        );
        assert!(union
            .observed_value(&json!({"id":true}), &cgs, "types")
            .is_err());
    }
}

#[cfg(test)]
mod dictionary_contract_tests {
    use super::*;
    #[test]
    fn dictionary_is_typed_open_data_without_record_guarantees() {
        let contract = ValueContract {
            shape: ValueShape::Dictionary {
                key: Box::new(ValueContract::scalar(FieldType::String)),
                value: Box::new(ValueContract::scalar(FieldType::Integer)),
            },
            domain: None,
            nullable: false,
        };
        assert!(!contract.is_non_null_record());
        assert!(contract.field("arbitrary").is_err());
        let validate = |json| {
            let value: crate::Value = serde_json::from_value(json).unwrap();
            contract.validate_in(&value, &CGS::default(), "fixture", "dictionary", &|_| None)
        };
        assert!(validate(serde_json::json!({})).is_ok());
        assert!(validate(serde_json::json!({"a":1,"b":2})).is_ok());
        assert!(validate(serde_json::json!({"a":true})).is_err());
        assert!(validate(serde_json::json!({"a":null})).is_err());
        assert!(validate(serde_json::json!({"a":"1"})).is_err());
        assert_eq!(
            serde_json::from_value::<ValueContract>(serde_json::to_value(&contract).unwrap())
                .unwrap(),
            contract
        );
    }
}
