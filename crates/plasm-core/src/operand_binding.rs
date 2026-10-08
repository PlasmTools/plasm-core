//! Structural operand substitution. Expression metadata never passes through a text or JSON evaluator.

mod data;

use crate::expr::{ChainStep, EntityKey, Expr, IdentitySlot};
use crate::{EntityId, EntityName, InvokeInputPayload, PlasmInputRef, Predicate, Value};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ResolvedValueError {
    #[error("resolved data contains an unresolved operand")]
    UnresolvedOperand,
    #[error("resolved data contains a non-finite number")]
    NonFiniteNumber,
    #[error("literal integer exceeds the signed 64-bit domain")]
    IntegerOutOfRange,
    #[error("wire number is not representable as a finite float")]
    InvalidFloat,
    #[error("could not serialize resolved value")]
    Serialization,
}

#[derive(Debug, Clone, Error)]
pub enum IdentityCodecError {
    #[error("unknown identity target entity `{entity}`")]
    TargetEntityNotFound { entity: String },
    #[error("entity `{entity}` has no declared identity slot `{field}`")]
    SlotNotDeclared { entity: String, field: String },
    #[error("entity `{entity}` requires a compound identity")]
    CompoundIdentityRequired { entity: String },
    #[error("could not resolve identity field type for `{entity}.{field}`")]
    FieldTypeResolution {
        entity: String,
        field: String,
        #[source]
        source: crate::ParentFieldTypeError,
    },
    #[error("identity field `{entity}.{field}` has unsupported type {field_type:?}")]
    UnsupportedFieldType {
        entity: String,
        field: String,
        field_type: crate::FieldType,
    },
    #[error("identity number must be finite")]
    NonFiniteNumber,
    #[error("identity is not a declared integer")]
    InvalidInteger,
    #[error("identity integer is outside the declared i64 domain")]
    IntegerOutOfRange,
    #[error("identity is not a declared number")]
    InvalidNumber,
    #[error("identity is not a declared boolean")]
    InvalidBoolean,
    #[error("identity operand must be a declared scalar")]
    ExpectedScalar,
    #[error("identity operand does not match declared {field_type:?} scalar domain")]
    ScalarDomainMismatch { field_type: crate::FieldType },
    #[error(transparent)]
    DigitId(#[from] crate::wire_coercion::DigitIdCoercionError),
    #[error("dry identity row is missing declared identity field `{field}`")]
    MissingIdentityValue { field: String },
}

/// Declared destination of an identity operand; compound slots retain their field name.
#[derive(Clone, Copy)]
pub struct IdentityTarget<'a> {
    pub entity: &'a EntityName,
    pub field: Option<&'a str>,
}

/// Serialize a floating identity scalar using the transport's canonical numeric spelling.
/// This is the identity-key encoding boundary, not a materialized row conversion.
pub fn encode_float_identity(value: f64) -> Result<String, IdentityCodecError> {
    serde_json::Number::from_f64(value)
        .map(|number| number.to_string())
        .ok_or(IdentityCodecError::NonFiniteNumber)
}

/// Target-directed identity encoder. Construction resolves the catalog identity slot;
/// encoding never converts an unsigned integer through floating point.
#[derive(Debug, Clone)]
pub struct IdentityCodec {
    field_type: crate::FieldType,
}

impl IdentityCodec {
    /// Lawful identity scalar types for `id_field` / `key_vars` (and dry-plan row identity).
    ///
    /// [`FieldType::EntityRef`] is a foreign-key shape — never a primary identity slot.
    /// Keep in sync with CGS validate (`UnsupportedIdentityType`).
    pub fn is_lawful_identity_type(field_type: &crate::FieldType) -> bool {
        matches!(
            field_type,
            crate::FieldType::String
                | crate::FieldType::Uuid
                | crate::FieldType::DigitId
                | crate::FieldType::Select
                | crate::FieldType::Integer
                | crate::FieldType::Number
                | crate::FieldType::Boolean
                | crate::FieldType::Date
        )
    }

    pub fn compile(
        cgs: &crate::CGS,
        target: IdentityTarget<'_>,
    ) -> Result<Self, IdentityCodecError> {
        let entity = cgs.get_entity(target.entity.as_str()).ok_or_else(|| {
            IdentityCodecError::TargetEntityNotFound {
                entity: target.entity.to_string(),
            }
        })?;
        let field = match target.field {
            Some(field) if entity.key_vars.iter().any(|key| key.as_str() == field) => field,
            Some(field) => {
                return Err(IdentityCodecError::SlotNotDeclared {
                    entity: target.entity.to_string(),
                    field: field.to_owned(),
                });
            }
            None if entity.key_vars.len() > 1 => {
                return Err(IdentityCodecError::CompoundIdentityRequired {
                    entity: target.entity.to_string(),
                });
            }
            None => entity.key_vars.first().unwrap_or(&entity.id_field).as_str(),
        };
        let field_type = crate::parent_entity_field_type(cgs, entity, field).map_err(|source| {
            IdentityCodecError::FieldTypeResolution {
                entity: target.entity.to_string(),
                field: field.to_owned(),
                source,
            }
        })?;
        if !Self::is_lawful_identity_type(&field_type) {
            return Err(IdentityCodecError::UnsupportedFieldType {
                entity: target.entity.to_string(),
                field: field.to_owned(),
                field_type,
            });
        }
        Ok(Self { field_type })
    }

    pub fn encode(&self, value: &Value) -> Result<EntityId, IdentityCodecError> {
        use crate::FieldType;
        if matches!(self.field_type, FieldType::DigitId) {
            return crate::wire_coercion::encode_digit_id_identity(value)
                .map(EntityId::from)
                .map_err(IdentityCodecError::from);
        }
        let numeric = || match value {
            Value::Integer(value) => Some(value.to_string()),
            Value::Unsigned(value) => Some(value.to_string()),
            Value::Float(value) if value.is_finite() => encode_float_identity(*value).ok(),
            _ => None,
        };
        let text = match (&self.field_type, value) {
            (
                FieldType::String | FieldType::Uuid | FieldType::Select | FieldType::Date,
                Value::String(value),
            ) => value.clone(),
            (FieldType::Integer, Value::String(value)) => value
                .parse::<i64>()
                .map_err(|_| IdentityCodecError::InvalidInteger)?
                .to_string(),
            (FieldType::Integer, value) => value
                .as_integer()
                .ok_or(IdentityCodecError::IntegerOutOfRange)?
                .to_string(),
            (FieldType::Number, Value::String(value)) => {
                if let Ok(number) = value.parse::<i64>() {
                    number.to_string()
                } else if let Ok(number) = value.parse::<u64>() {
                    number.to_string()
                } else {
                    let number = value
                        .parse::<f64>()
                        .map_err(|_| IdentityCodecError::InvalidNumber)?;
                    if !number.is_finite() {
                        return Err(IdentityCodecError::NonFiniteNumber);
                    }
                    encode_float_identity(number)?
                }
            }
            (FieldType::Boolean, Value::Bool(value)) => value.to_string(),
            (FieldType::Boolean, Value::String(value)) => value
                .parse::<bool>()
                .map_err(|_| IdentityCodecError::InvalidBoolean)?
                .to_string(),
            (FieldType::String | FieldType::Select | FieldType::Number | FieldType::Date, _) => {
                numeric().ok_or(IdentityCodecError::ExpectedScalar)?
            }
            _ => {
                return Err(IdentityCodecError::ScalarDomainMismatch {
                    field_type: self.field_type.clone(),
                });
            }
        };
        Ok(EntityId::from(text))
    }
}

/// Data accepted from a resolver cannot contain another executable reference.
///
/// ```compile_fail
/// use plasm_core::{Value, operand_binding::ResolvedValue};
/// let unchecked = ResolvedValue(Value::Null);
/// ```
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(into = "Value")]
pub struct ResolvedValue(Value);

impl<'de> serde::Deserialize<'de> for ResolvedValue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = <serde_json::Value as serde::Deserialize>::deserialize(deserializer)?;
        Self::from_wire(wire).map_err(serde::de::Error::custom)
    }
}

// Construction excludes non-finite floats, so equality is reflexive.
impl Eq for ResolvedValue {}

impl TryFrom<Value> for ResolvedValue {
    type Error = ResolvedValueError;
    fn try_from(value: Value) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<ResolvedValue> for Value {
    fn from(value: ResolvedValue) -> Self {
        value.into_value()
    }
}

impl ResolvedValue {
    /// Construct a resolved string, which cannot contain executable operands or invalid numbers.
    #[must_use]
    pub fn string(value: impl Into<String>) -> Self {
        Self(Value::String(value.into()))
    }

    #[must_use]
    pub fn boolean(value: bool) -> Self {
        Self(Value::Bool(value))
    }

    pub fn null() -> Self {
        Self(Value::Null)
    }
    pub fn new(value: Value) -> Result<Self, ResolvedValueError> {
        fn check(value: &Value) -> Result<(), ResolvedValueError> {
            match value {
                Value::PlasmInputRef(_)
                | Value::GetScalarExtract(_)
                | Value::PhraseIdent(_)
                | Value::StringTemplate(_)
                | Value::UnionCtor { .. } => Err(ResolvedValueError::UnresolvedOperand),
                Value::Array(items) => items.iter().try_for_each(check),
                Value::Object(fields) => fields.values().try_for_each(check),
                Value::Float(value) if !value.is_finite() => {
                    Err(ResolvedValueError::NonFiniteNumber)
                }
                Value::Null
                | Value::Bool(_)
                | Value::Unsigned(_)
                | Value::Integer(_)
                | Value::String(_)
                | Value::Money(_)
                | Value::Float(_) => Ok(()),
            }
        }
        check(&value)?;
        Ok(Self(value))
    }
    pub fn as_array(&self) -> Option<&[Value]> {
        if let Value::Array(items) = &self.0 {
            Some(items)
        } else {
            None
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        if let Value::String(value) = &self.0 {
            Some(value)
        } else {
            None
        }
    }
    pub fn value(&self) -> &Value {
        &self.0
    }
    pub fn to_wire(&self) -> Result<serde_json::Value, ResolvedValueError> {
        serde_json::to_value(&self.0).map_err(|_| ResolvedValueError::Serialization)
    }
    pub fn from_wire(value: serde_json::Value) -> Result<Self, ResolvedValueError> {
        fn decode(value: serde_json::Value) -> Result<Value, ResolvedValueError> {
            use serde_json::Value as J;
            Ok(match value {
                J::Null => Value::Null,
                J::Bool(v) => Value::Bool(v),
                J::Number(v) => {
                    if let Some(i) = v.as_i64() {
                        Value::Integer(i)
                    } else if let Some(i) = v.as_u64() {
                        Value::Unsigned(i)
                    } else if v.is_f64() {
                        Value::Float(v.as_f64().ok_or(ResolvedValueError::InvalidFloat)?)
                    } else {
                        return Err(ResolvedValueError::IntegerOutOfRange);
                    }
                }
                J::String(v) => Value::String(v),
                J::Array(items) => {
                    Value::Array(items.into_iter().map(decode).collect::<Result<_, _>>()?)
                }
                J::Object(fields) => {
                    if let Some(money) = crate::money::try_from_json_object(&fields) {
                        Value::Money(money)
                    } else {
                        Value::Object(
                            fields
                                .into_iter()
                                .map(|(k, v)| Ok((k, decode(v)?)))
                                .collect::<Result<_, ResolvedValueError>>()?,
                        )
                    }
                }
            })
        }
        Self::new(decode(value)?)
    }
    pub fn into_value(self) -> Value {
        self.0
    }
}

/// Supplies data to structural operands. Implementations cannot change expression structure.
pub trait OperandResolver {
    type Error;
    fn resolve(&mut self, reference: &PlasmInputRef) -> Result<ResolvedValue, Self::Error>;
    fn node(
        &mut self,
        node: &str,
        _alias: &str,
        path: &[String],
    ) -> Result<ResolvedValue, Self::Error> {
        self.resolve(&PlasmInputRef::node_output(node, path.to_vec()))
    }
    fn template(
        &mut self,
        value: &crate::program_string_template::CompiledProgramString,
        _bindings: &[crate::PlanInputBinding],
    ) -> Result<String, Self::Error> {
        self.string(value)
    }
    fn identity(
        &mut self,
        target: IdentityTarget<'_>,
        reference: &PlasmInputRef,
    ) -> Result<EntityId, Self::Error>;
    fn string(
        &mut self,
        value: &crate::program_string_template::CompiledProgramString,
    ) -> Result<String, Self::Error>;
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for crate::Expr {}
    impl Sealed for crate::PlasmDataValue {}
    impl Sealed for crate::PlanPredicate {}
    impl Sealed for crate::Value {}
    impl Sealed for crate::Predicate {}
    impl Sealed for crate::InvokeInputPayload {}
    impl Sealed for crate::TypedInvokeInput {}
}

/// One exhaustive traversal owns binding for every executable expression and operand kind.
pub trait BindOperands: sealed::Sealed + Sized {
    fn bind_operands<R: OperandResolver>(&self, resolver: &mut R) -> Result<Self, R::Error>;
}

impl BindOperands for InvokeInputPayload {
    fn bind_operands<R: OperandResolver>(&self, resolver: &mut R) -> Result<Self, R::Error> {
        Ok(match self {
            Self::Raw(value) => Self::Raw(value.bind_operands(resolver)?),
            Self::Typed(value) => Self::Typed(value.bind_operands(resolver)?),
        })
    }
}

impl BindOperands for crate::TypedInvokeInput {
    fn bind_operands<R: OperandResolver>(&self, resolver: &mut R) -> Result<Self, R::Error> {
        Ok(match self {
            Self::Leaf(value) => bind_typed_literal(value, resolver)?,
            Self::PlasmInputRef(reference) => Self::Json(resolver.resolve(reference)?.into_value()),
            Self::Json(value) => Self::Json(value.bind_operands(resolver)?),
            Self::Array(items) => Self::Array(
                items
                    .iter()
                    .map(|value| value.bind_operands(resolver))
                    .collect::<Result<_, _>>()?,
            ),
            Self::Object { fields, extra } => Self::Object {
                fields: fields
                    .iter()
                    .map(|(key, value)| Ok((key.clone(), value.bind_operands(resolver)?)))
                    .collect::<Result<_, R::Error>>()?,
                extra: extra
                    .as_ref()
                    .map(|fields| {
                        fields
                            .iter()
                            .map(|(key, value)| Ok((key.clone(), value.bind_operands(resolver)?)))
                            .collect::<Result<_, R::Error>>()
                    })
                    .transpose()?,
            },
            Self::Union {
                variant_index,
                wire_field,
                wire_value,
                value,
                nested_wire_paths,
                array_element_wrap_keys,
            } => Self::Union {
                variant_index: *variant_index,
                wire_field: wire_field.clone(),
                wire_value: wire_value.clone(),
                value: Box::new(value.bind_operands(resolver)?),
                nested_wire_paths: nested_wire_paths.clone(),
                array_element_wrap_keys: array_element_wrap_keys.clone(),
            },
        })
    }
}

fn bind_typed_literal<R: OperandResolver>(
    value: &crate::TypedLiteral,
    resolver: &mut R,
) -> Result<crate::TypedInvokeInput, R::Error> {
    use crate::{TypedInvokeInput, TypedLiteral};
    Ok(match value {
        TypedLiteral::InputRef(reference) => {
            TypedInvokeInput::Json(resolver.resolve(reference)?.into_value())
        }
        TypedLiteral::StringTemplate(template) => {
            TypedInvokeInput::Leaf(TypedLiteral::String(resolver.string(template)?))
        }
        TypedLiteral::Array(items) => TypedInvokeInput::Array(
            items
                .iter()
                .map(|value| bind_typed_literal(value, resolver))
                .collect::<Result<_, _>>()?,
        ),
        TypedLiteral::Null
        | TypedLiteral::Bool(_)
        | TypedLiteral::Integer(_)
        | TypedLiteral::Float(_)
        | TypedLiteral::String(_)
        | TypedLiteral::EntityRef(_)
        | TypedLiteral::Money(_) => TypedInvokeInput::Leaf(value.clone()),
    })
}

impl BindOperands for Value {
    fn bind_operands<R: OperandResolver>(&self, resolver: &mut R) -> Result<Self, R::Error> {
        Ok(match self {
            Self::PlasmInputRef(reference) => resolver.resolve(reference)?.into_value(),
            Self::GetScalarExtract(extract) => Self::GetScalarExtract(crate::GetScalarExtract {
                entity: extract.entity.clone(),
                identity: Box::new(extract.identity.bind_operands(resolver)?),
                wire: extract.wire.clone(),
                catalog_entry_id: extract.catalog_entry_id.clone(),
            }),
            Self::StringTemplate(value) => Self::String(resolver.string(value)?),
            Self::Array(items) => Self::Array(
                items
                    .iter()
                    .map(|v| v.bind_operands(resolver))
                    .collect::<Result<_, _>>()?,
            ),
            Self::Object(fields) => Self::Object(
                fields
                    .iter()
                    .map(|(k, v)| Ok((k.clone(), v.bind_operands(resolver)?)))
                    .collect::<Result<_, R::Error>>()?,
            ),
            Self::UnionCtor {
                ctor_label,
                ctor_fields,
            } => Self::UnionCtor {
                ctor_label: ctor_label.clone(),
                ctor_fields: ctor_fields
                    .iter()
                    .map(|(k, v)| Ok((k.clone(), v.bind_operands(resolver)?)))
                    .collect::<Result<_, R::Error>>()?,
            },
            Self::String(_)
            | Self::Null
            | Self::Bool(_)
            | Self::Unsigned(_)
            | Self::Integer(_)
            | Self::Float(_)
            | Self::PhraseIdent(_)
            | Self::Money(_) => self.clone(),
        })
    }
}

impl BindOperands for Predicate {
    fn bind_operands<R: OperandResolver>(&self, resolver: &mut R) -> Result<Self, R::Error> {
        Ok(match self {
            Self::True | Self::False => self.clone(),
            Self::Comparison { field, op, value } => Self::Comparison {
                field: field.clone(),
                op: *op,
                value: value.to_value().bind_operands(resolver)?.into(),
            },
            Self::And { args } => Self::And {
                args: args
                    .iter()
                    .map(|p| p.bind_operands(resolver))
                    .collect::<Result<_, _>>()?,
            },
            Self::Or { args } => Self::Or {
                args: args
                    .iter()
                    .map(|p| p.bind_operands(resolver))
                    .collect::<Result<_, _>>()?,
            },
            Self::Not { predicate } => Self::Not {
                predicate: Box::new(predicate.bind_operands(resolver)?),
            },
            Self::ExistsRelation {
                relation,
                predicate,
            } => Self::ExistsRelation {
                relation: relation.clone(),
                predicate: predicate
                    .as_ref()
                    .map(|p| p.bind_operands(resolver).map(Box::new))
                    .transpose()?,
            },
        })
    }
}

fn bind_key<R: OperandResolver>(
    entity: &EntityName,
    key: &mut EntityKey,
    resolver: &mut R,
) -> Result<(), R::Error> {
    let mut bind_slot = |field, slot: &mut IdentitySlot| {
        if let IdentitySlot::Binding(reference) = slot {
            *slot =
                IdentitySlot::Lit(resolver.identity(IdentityTarget { entity, field }, reference)?);
        }
        Ok(())
    };
    match key {
        EntityKey::Simple(slot) => bind_slot(None, slot),
        EntityKey::Compound(slots) => slots
            .iter_mut()
            .try_for_each(|(field, slot)| bind_slot(Some(field.as_str()), slot)),
    }
}

fn bind_input<R: OperandResolver>(
    input: &mut Option<InvokeInputPayload>,
    resolver: &mut R,
) -> Result<(), R::Error> {
    if let Some(value) = input {
        *value = value.bind_operands(resolver)?;
    }
    Ok(())
}

impl BindOperands for Expr {
    fn bind_operands<R: OperandResolver>(&self, resolver: &mut R) -> Result<Self, R::Error> {
        let mut bound = self.clone();
        match &mut bound {
            Self::Query(query) => {
                query.predicate = query
                    .predicate
                    .as_ref()
                    .map(|p| p.bind_operands(resolver))
                    .transpose()?;
            }
            Self::Get(get) => {
                bind_key(&get.reference.entity_type, &mut get.reference.key, resolver)?;
                bind_input(&mut get.input, resolver)?;
            }
            Self::Create(create) => {
                create.input = create.input.bind_operands(resolver)?;
                create.dotted_receiver = create
                    .dotted_receiver
                    .as_ref()
                    .map(|e| e.bind_operands(resolver).map(Box::new))
                    .transpose()?;
            }
            Self::Delete(delete) => {
                bind_key(&delete.target.entity_type, &mut delete.target.key, resolver)?;
                bind_input(&mut delete.input, resolver)?;
            }
            Self::Invoke(invoke) => {
                bind_key(&invoke.target.entity_type, &mut invoke.target.key, resolver)?;
                bind_input(&mut invoke.input, resolver)?;
            }
            Self::Chain(chain) => {
                *chain.source = chain.source.bind_operands(resolver)?;
                match &mut chain.step {
                    ChainStep::AutoGet => {}
                    ChainStep::Explicit { expr } => {
                        **expr = expr.bind_operands(resolver)?;
                    }
                }
            }
            Self::TeachingValue { value } => {
                *value = value.bind_operands(resolver)?;
            }
            Self::Page(_) | Self::Wait(_) | Self::Cancel(_) => {}
        }
        Ok(bound)
    }
}

/// Inspect references through the same exhaustive structural traversal used for binding.
pub fn input_references(expr: &Expr) -> Vec<PlasmInputRef> {
    struct References(Vec<PlasmInputRef>);
    impl OperandResolver for References {
        type Error = std::convert::Infallible;
        fn resolve(&mut self, reference: &PlasmInputRef) -> Result<ResolvedValue, Self::Error> {
            self.0.push(reference.clone());
            Ok(ResolvedValue(Value::Null))
        }
        fn identity(
            &mut self,
            _target: IdentityTarget<'_>,
            reference: &PlasmInputRef,
        ) -> Result<EntityId, Self::Error> {
            self.0.push(reference.clone());
            Ok(EntityId::from("reference-inspection"))
        }
        fn string(
            &mut self,
            value: &crate::program_string_template::CompiledProgramString,
        ) -> Result<String, Self::Error> {
            Ok(value.source().to_owned())
        }
    }
    let mut references = References(Vec::new());
    let Ok(_) = expr.bind_operands(&mut references);
    references.0
}

/// Structural call inputs for policy analysis; operation metadata is never inspected as JSON.
pub fn invocation_input(expr: &Expr) -> Option<Value> {
    match expr {
        Expr::Get(c) => c.input.as_ref().map(InvokeInputPayload::to_value),
        Expr::Create(c) => Some(c.input.to_value()),
        Expr::Invoke(c) => c.input.as_ref().map(InvokeInputPayload::to_value),
        Expr::Delete(c) => c.input.as_ref().map(InvokeInputPayload::to_value),
        Expr::Chain(c) => match &c.step {
            ChainStep::Explicit { expr } => invocation_input(expr),
            ChainStep::AutoGet => None,
        },
        Expr::Query(_)
        | Expr::Page(_)
        | Expr::Wait(_)
        | Expr::Cancel(_)
        | Expr::TeachingValue { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr_parser::parse;

    #[test]
    fn float_identity_preserves_wire_scalar_spelling() {
        for value in [1.0, -0.0, 1e30, 1e-30, 1.25] {
            assert_eq!(
                encode_float_identity(value).unwrap(),
                serde_json::to_string(&value).unwrap()
            );
        }
        assert!(encode_float_identity(f64::INFINITY).is_err());
        assert!(encode_float_identity(f64::NAN).is_err());
    }

    fn matrix() -> crate::CGS {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        crate::loader::load_schema_dir(&path).expect("matrix")
    }

    #[test]
    fn literal_strings_are_not_executable_templates() {
        #[derive(Debug, thiserror::Error)]
        enum UnexpectedResolution {
            #[error("unexpected reference")]
            Reference,
            #[error("unexpected identity")]
            Identity,
            #[error("unexpected template")]
            Template,
        }
        struct Reject;
        impl OperandResolver for Reject {
            type Error = UnexpectedResolution;
            fn resolve(&mut self, _: &PlasmInputRef) -> Result<ResolvedValue, Self::Error> {
                Err(UnexpectedResolution::Reference)
            }
            fn identity(
                &mut self,
                _: IdentityTarget<'_>,
                _: &PlasmInputRef,
            ) -> Result<EntityId, Self::Error> {
                Err(UnexpectedResolution::Identity)
            }
            fn string(
                &mut self,
                _: &crate::program_string_template::CompiledProgramString,
            ) -> Result<String, Self::Error> {
                Err(UnexpectedResolution::Template)
            }
        }
        let literal = Value::String("{{ missing }} ${ordinary data}".into());
        assert_eq!(literal.bind_operands(&mut Reject).unwrap(), literal);
    }

    #[test]
    fn source_templates_compile_before_binding() {
        let cgs = matrix();
        let parsed = parse("LangItem(42).update(title=\"{{ supplied }}\")", &cgs).unwrap();
        let input = invocation_input(&parsed.expr).unwrap();
        assert!(matches!(
            input.as_object().unwrap().get("title"),
            Some(Value::StringTemplate(_))
        ));
        assert!(parse("LangItem(42).update(title=\"{{ broken\")", &cgs).is_err());
        let Value::StringTemplate(template) =
            Value::program_string("{{ supplied }}".into()).unwrap()
        else {
            panic!("compiled template")
        };
        let scope = std::collections::BTreeMap::from([(
            "supplied".into(),
            Value::String("{{ still data }}".into()),
        )]);
        assert_eq!(template.render(&scope).unwrap(), "{{ still data }}");
    }

    #[test]
    fn identity_codec_preserves_large_integers_and_rejects_wrong_domains() {
        let string = IdentityCodec {
            field_type: crate::FieldType::String,
        };
        assert_eq!(
            string
                .encode(&crate::fixture_value!(u64::MAX))
                .unwrap()
                .as_str(),
            "18446744073709551615"
        );
        assert!(string.encode(&crate::fixture_value!(true)).is_err());
        assert!(string.encode(&crate::fixture_value!([42])).is_err());
        let integer = IdentityCodec {
            field_type: crate::FieldType::Integer,
        };
        assert_eq!(
            integer
                .encode(&crate::fixture_value!(i64::MAX))
                .unwrap()
                .as_str(),
            "9223372036854775807"
        );
        assert!(integer.encode(&crate::fixture_value!(u64::MAX)).is_err());
        assert!(integer.encode(&crate::fixture_value!(42.5)).is_err());
        assert!(integer.encode(&Value::Null).is_err());
    }

    #[test]
    fn canonical_identity_strings_match_explicit_scalar_fields() {
        for (field_type, scalar, canonical) in [
            (crate::FieldType::Integer, crate::fixture_value!(42), "42"),
            (
                crate::FieldType::Number,
                crate::fixture_value!(42.5),
                "42.5",
            ),
            (
                crate::FieldType::Boolean,
                crate::fixture_value!(true),
                "true",
            ),
        ] {
            let codec = IdentityCodec { field_type };
            assert_eq!(
                codec.encode(&scalar).unwrap(),
                codec.encode(&crate::fixture_value!(canonical)).unwrap()
            );
        }
    }

    #[test]
    fn identity_codec_requires_a_declared_target_slot() {
        let cgs = matrix();
        let entity = crate::EntityName::from("LangItem");
        assert!(IdentityCodec::compile(
            &cgs,
            IdentityTarget {
                entity: &entity,
                field: None
            }
        )
        .is_ok());
        assert!(IdentityCodec::compile(
            &cgs,
            IdentityTarget {
                entity: &entity,
                field: Some("invented")
            }
        )
        .is_err());
    }

    fn date_identity_cgs() -> crate::CGS {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/date_identity");
        crate::loader::load_schema_dir(&path).expect("date_identity fixture")
    }

    fn digit_id_identity_cgs() -> crate::CGS {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/digit_id_identity");
        crate::loader::load_schema_dir(&path).expect("digit_id_identity fixture")
    }

    fn entity_ref_as_identity_cgs_unvalidated() -> crate::CGS {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/entity_ref_as_identity");
        crate::loader::load_schema_dir_unvalidated(&path).expect("entity_ref_as_identity fixture")
    }

    #[test]
    fn identity_codec_rejects_entity_ref_primary_key() {
        // Catalog authoring heresy: id_field typed as entity_ref to self.
        // Dry staging / Get binding must refuse — same failure as amazon Product.product_id.
        let cgs = entity_ref_as_identity_cgs_unvalidated();
        let entity = crate::EntityName::from("BadProduct");
        let err = IdentityCodec::compile(
            &cgs,
            IdentityTarget {
                entity: &entity,
                field: None,
            },
        )
        .expect_err("entity_ref id_field is not a lawful identity scalar");
        assert!(matches!(
            err,
            IdentityCodecError::UnsupportedFieldType {
                field_type: crate::FieldType::EntityRef { .. },
                ..
            }
        ));
    }

    #[test]
    fn identity_codec_date_identity_encodes_temporal_wire() {
        let codec = IdentityCodec {
            field_type: crate::FieldType::Date,
        };
        assert_eq!(
            codec
                .encode(&crate::fixture_value!("2020-01-01T00:00:00Z"))
                .unwrap()
                .as_str(),
            "2020-01-01T00:00:00Z"
        );
        assert_eq!(
            codec
                .encode(&crate::fixture_value!(1_577_836_800_000i64))
                .unwrap()
                .as_str(),
            "1577836800000"
        );
        assert!(codec.encode(&crate::fixture_value!(true)).is_err());
    }

    #[test]
    fn identity_codec_digit_id_encodes_exact_digits_rejects_float() {
        let codec = IdentityCodec {
            field_type: crate::FieldType::DigitId,
        };
        assert_eq!(
            codec
                .encode(&crate::fixture_value!("6419671322388907"))
                .unwrap()
                .as_str(),
            "6419671322388907"
        );
        assert_eq!(
            codec
                .encode(&crate::fixture_value!(6_419_671_322_388_907i64))
                .unwrap()
                .as_str(),
            "6419671322388907"
        );
        assert!(matches!(
            codec
                .encode(&crate::fixture_value!(9_007_199_254_740_993i64 as f64))
                .unwrap_err(),
            IdentityCodecError::DigitId(
                crate::wire_coercion::DigitIdCoercionError::InexactFloatIdentity
            )
        ));
    }

    #[test]
    fn digit_id_query_and_where_compile() {
        let cgs = digit_id_identity_cgs();
        let entity = crate::EntityName::from("DigitAccount");
        let codec = IdentityCodec::compile(
            &cgs,
            IdentityTarget {
                entity: &entity,
                field: None,
            },
        )
        .expect("digit_id id_field is a lawful identity scalar");
        let ent = cgs.get_entity("DigitAccount").expect("DigitAccount");
        let rows = crate::dry_stub_entity_rows(&cgs, ent, 1).expect("RA-8 digit_id stubs");
        codec
            .encode(rows[0].get("pan").expect("dry stub retains digit_id"))
            .expect("dry digit_id identity encodes");

        let queried =
            parse(r#"DigitAccount{access_token="tok"}"#, &cgs).expect("taught query form");
        crate::type_checker::type_check_expr(&queried.expr, &cgs).expect("query type-checks");

        let shape = crate::expr_parser::parse_program_shape(
            r#"DigitAccount{access_token="tok"} | where pan = "6419671322388907""#,
        )
        .expect("quoted digit_id where is a pipe program");
        match &shape.roots[0].row {
            crate::expr_parser::RowExpr::Pipe(p) => {
                assert!(
                    p.stages.iter().any(|s| matches!(
                        s,
                        crate::expr_parser::PipeStage::Where { predicates }
                        if predicates.contains("6419671322388907")
                    )),
                    "expected residual | where on pan, got {p:?}"
                );
            }
            other => panic!("expected pipe, got {other:?}"),
        }
    }

    #[test]
    fn date_identity_query_dry_stub_encodes() {
        let cgs = date_identity_cgs();
        let entity = crate::EntityName::from("DateLedger");
        let codec = IdentityCodec::compile(
            &cgs,
            IdentityTarget {
                entity: &entity,
                field: None,
            },
        )
        .expect("Date id_field is a lawful identity scalar");
        let ent = cgs.get_entity("DateLedger").expect("DateLedger");
        let rows = crate::dry_stub_entity_rows(&cgs, ent, 2).expect("RA-8 Date stubs");
        for row in &rows {
            let value = row
                .get("occurred_at")
                .expect("dry stub retains Date identity");
            codec.encode(value).expect("dry Date identity encodes");
        }
        let parsed = parse(r#"DateLedger{access_token="tok"}"#, &cgs).expect("taught query form");
        crate::type_checker::type_check_expr(&parsed.expr, &cgs).expect("query type-checks");
    }

    #[test]
    fn typed_delete_preserves_and_requires_declared_input() {
        let mut cgs = matrix();
        let inputs = cgs.capabilities["langitem_secured_touch"].inputs.clone();
        cgs.capabilities.get_mut("langitem_delete").unwrap().inputs = inputs;
        let parsed =
            parse("LangItem(42).delete(access_token=\"fixture-value\")", &cgs).expect("parse");
        let Expr::Delete(delete) = &parsed.expr else {
            panic!("delete expected")
        };
        assert_eq!(
            delete
                .input
                .as_ref()
                .unwrap()
                .to_value()
                .as_object()
                .unwrap()["access_token"]
                .as_str(),
            Some("fixture-value")
        );
        crate::type_checker::type_check_expr(&parsed.expr, &cgs).expect("supplied input");
        let mut missing = delete.clone();
        missing.input = None;
        assert!(crate::type_checker::type_check_delete(&missing, &cgs).is_err());
    }

    #[test]
    fn resolved_values_reject_nested_executable_references() {
        let reference = Value::PlasmInputRef(PlasmInputRef::node_output("source", vec![]));
        assert!(matches!(
            ResolvedValue::new(Value::Array(vec![reference])),
            Err(ResolvedValueError::UnresolvedOperand)
        ));
        assert!(matches!(
            ResolvedValue::new(Value::Float(f64::NAN)),
            Err(ResolvedValueError::NonFiniteNumber)
        ));
        assert_eq!(
            ResolvedValue::new(Value::String("resolved".into()))
                .expect("string is resolved")
                .to_wire()
                .expect("resolved values serialize"),
            serde_json::json!("resolved")
        );
    }

    #[test]
    fn typed_binding_never_interprets_metadata_or_returned_data() {
        struct Data;
        impl OperandResolver for Data {
            type Error = std::convert::Infallible;
            fn resolve(&mut self, _: &PlasmInputRef) -> Result<ResolvedValue, Self::Error> {
                Ok(ResolvedValue::new(Value::Object(indexmap::IndexMap::from([(
                    "__plasm_hole".into(),
                    Value::String("literal data".into()),
                )])))
                .unwrap())
            }
            fn identity(
                &mut self,
                _: IdentityTarget<'_>,
                _: &PlasmInputRef,
            ) -> Result<EntityId, Self::Error> {
                Ok(EntityId::from("42"))
            }
            fn string(
                &mut self,
                _: &crate::program_string_template::CompiledProgramString,
            ) -> Result<String, Self::Error> {
                panic!("no string operand")
            }
        }
        let mut delete = crate::DeleteExpr::with_target(
            "literal{{capability}}",
            crate::Ref::new("Item", "fixed"),
        );
        delete.input =
            Some(Value::PlasmInputRef(PlasmInputRef::node_output("source", vec![])).into());
        let result = Expr::Delete(delete).bind_operands(&mut Data).unwrap();
        let Expr::Delete(result) = result else {
            panic!("delete")
        };
        assert_eq!(result.capability.as_str(), "literal{{capability}}");
        assert!(result
            .input
            .unwrap()
            .to_value()
            .as_object()
            .unwrap()
            .contains_key("__plasm_hole"));

        let typed = InvokeInputPayload::Typed(crate::TypedInvokeInput::Union {
            variant_index: 2,
            wire_field: "kind".into(),
            wire_value: "record".into(),
            value: Box::new(crate::TypedInvokeInput::Object {
                fields: indexmap::IndexMap::from([(
                    "content".into(),
                    crate::TypedInvokeInput::PlasmInputRef(PlasmInputRef::node_output(
                        "source",
                        vec![],
                    )),
                )]),
                extra: None,
            }),
            nested_wire_paths: indexmap::IndexMap::from([(
                "content".into(),
                vec!["body".into(), "content".into()],
            )]),
            array_element_wrap_keys: indexmap::IndexMap::new(),
        });
        let bound = typed.bind_operands(&mut Data).unwrap();
        let InvokeInputPayload::Typed(crate::TypedInvokeInput::Union {
            variant_index,
            nested_wire_paths,
            ..
        }) = &bound
        else {
            panic!("binding must retain compiled union structure")
        };
        assert_eq!(*variant_index, 2);
        assert_eq!(nested_wire_paths["content"], ["body", "content"]);
        let wire = bound.to_value();
        assert_eq!(wire.as_object().unwrap()["kind"].as_str(), Some("record"));
        assert!(
            wire.as_object().unwrap()["body"].as_object().unwrap()["content"]
                .as_object()
                .unwrap()
                .contains_key("__plasm_hole")
        );
    }
}
