//! Fallible, schema-directed wire codecs. Program syntax is not wire data.
use crate::{FieldType, NamedValueSchema, Value};

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedValueContract {
    schema: NamedValueSchema,
    nullable: bool,
    pin: Option<crate::value_contract::DomainRef>,
    element: Option<Box<ResolvedValueContract>>,
    identity: Option<Vec<(String, ResolvedValueContract)>>,
}

impl ResolvedValueContract {
    pub(crate) fn new(schema: &NamedValueSchema, nullable: bool) -> Result<Self, ValueCodecError> {
        if schema.field_type != schema.domain.to_field_type()
            || schema.value_format != schema.domain.to_value_format()
            || schema.currency != schema.domain.currency
            || schema.allowed_values
                != schema
                    .domain
                    .enum_membership
                    .as_ref()
                    .map(|members| members.tokens().to_vec())
        {
            return Err(ValueCodecError::InconsistentContract);
        }
        Ok(Self {
            schema: schema.clone(),
            nullable,
            pin: None,
            element: None,
            identity: None,
        })
    }
    /// Resolve the existing catalog contract, including recursive elements and identity slots.
    pub fn resolve(
        cgs: &crate::CGS,
        entry: &str,
        key: &crate::ValueDomainKey,
        nullable: bool,
    ) -> Result<Self, ValueCodecError> {
        let contract = crate::value_contract::ValueContract::from_domain(cgs, entry, key)?;
        Self::resolve_contract(cgs, entry, &contract, nullable, 0)
    }

    fn resolve_contract(
        cgs: &crate::CGS,
        entry: &str,
        contract: &crate::value_contract::ValueContract,
        nullable: bool,
        depth: usize,
    ) -> Result<Self, ValueCodecError> {
        use crate::value_contract::{ValueContractError, ValueShape};
        if depth >= 64 {
            return Err(ValueContractError::DomainResolutionDepthExceeded.into());
        }
        let pin = contract
            .domain
            .as_ref()
            .ok_or(ValueContractError::DomainNotFound)?;
        let schema = cgs
            .values
            .get(pin.value_ref.as_str())
            .ok_or(ValueContractError::DomainNotFound)?;
        let mut resolved = Self::new(schema, nullable)?;
        resolved.pin = Some(pin.clone());
        if let ValueShape::Array { element } = &contract.shape {
            resolved.element = Some(Box::new(Self::resolve_contract(
                cgs,
                entry,
                element,
                element.nullable,
                depth + 1,
            )?));
        }
        if let FieldType::EntityRef { entry_id, target } = &schema.field_type {
            if !entry_id.as_str().is_empty() && entry_id.as_str() != entry {
                return Err(ValueContractError::ReferenceCatalogNotLoaded {
                    entry_id: entry_id.to_string(),
                }
                .into());
            }
            let target = cgs.get_entity(target.as_str()).ok_or_else(|| {
                ValueContractError::ReferenceTargetMissing {
                    target: target.to_string(),
                }
            })?;
            let keys = if target.key_vars.is_empty() {
                vec![target.id_field.clone()]
            } else {
                target.key_vars.clone()
            };
            let mut identity = Vec::with_capacity(keys.len());
            for key in keys {
                let member = if let Some(field) = target.fields.get(key.as_str()) {
                    let member = crate::value_contract::ValueContract::from_domain(
                        cgs,
                        entry,
                        field.kind.registry_key(),
                    )?;
                    Self::resolve_contract(cgs, entry, &member, false, depth + 1)?
                } else {
                    let domain = crate::value_domain::ValueDomain::new(
                        crate::value_domain::KernelKind::String,
                        None,
                        Default::default(),
                        None,
                        None,
                    )
                    .expect("unconstrained identity string");
                    Self::new(
                        &NamedValueSchema::from_domain(String::new(), domain, None),
                        false,
                    )?
                };
                if !crate::operand_binding::IdentityCodec::is_lawful_identity_type(
                    &member.schema.field_type,
                ) {
                    return Err(ValueCodecError::IdentitySlot {
                        expected: member.schema.field_type,
                    });
                }
                identity.push((key.to_string(), member));
            }
            resolved.identity = Some(identity);
        }
        Ok(resolved)
    }
}

/// Only a codec can construct this value; its contract travels with the payload.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedValue {
    contract: ResolvedValueContract,
    value: Value,
}

impl ValidatedValue {
    pub fn value(&self) -> &Value {
        &self.value
    }
    pub fn into_value(self) -> Value {
        self.value
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValueCodecError {
    #[error("invalid structural storage tag")]
    InvalidStorageTag,
    #[error(transparent)]
    ConcreteValue(Box<crate::operand_binding::ResolvedValueError>),
    #[error(transparent)]
    Currency(#[from] crate::money::CrossCurrencyError),
    #[error("named value type and domain disagree")]
    InconsistentContract,
    #[error(transparent)]
    Contract(#[from] crate::value_contract::ValueContractError),
    #[error("invalid identity slot for {expected}")]
    IdentitySlot { expected: FieldType },
    #[error(transparent)]
    IdentitySchema(#[from] crate::ParentFieldTypeError),
    #[error("value was validated under a different contract")]
    ContractMismatch,
    #[error("wire profile cannot preserve the value under this contract")]
    LossyWireRepresentation,
    #[error("null is not admitted by this contract")]
    Null,
    #[error("expected {expected}, received {actual}")]
    Shape {
        expected: String,
        actual: &'static str,
    },
    #[error("array element {index}: {source}")]
    Element {
        index: usize,
        #[source]
        source: Box<ValueCodecError>,
    },
    #[error("field `{field}`: {source}")]
    Field {
        field: String,
        #[source]
        source: Box<ValueCodecError>,
    },
    #[error(transparent)]
    Encoding(#[from] crate::WireEncodingError),
    #[error(transparent)]
    Reference(#[from] crate::entity_ref_value::EntityRefValueError),
    #[error(transparent)]
    Coercion(#[from] crate::CoercionError),
    #[error(transparent)]
    Domain(#[from] crate::ValueDomainViolation),
}

pub trait ValueCodec {
    type Wire;
    fn decode(
        &self,
        contract: &ResolvedValueContract,
        wire: Self::Wire,
    ) -> Result<ValidatedValue, ValueCodecError>;
    fn encode(
        &self,
        contract: &ResolvedValueContract,
        value: &ValidatedValue,
    ) -> Result<Self::Wire, ValueCodecError>;
}

#[derive(Debug, Clone, Copy)]
pub struct ValueWireCodec;

impl ValueCodec for ValueWireCodec {
    type Wire = Value;
    fn decode(
        &self,
        contract: &ResolvedValueContract,
        wire: Value,
    ) -> Result<ValidatedValue, ValueCodecError> {
        // Reject deferred operands and non-finite values, including nested opaque data.
        validate_concrete_tree(&wire)?;
        let value = if wire.is_null() {
            if !contract.nullable {
                return Err(ValueCodecError::Null);
            }
            wire
        } else if let Some(element) = &contract.element {
            let Value::Array(values) = wire else {
                return Err(ValueCodecError::Shape {
                    expected: "array".into(),
                    actual: wire.type_name(),
                });
            };
            Value::Array(
                values
                    .into_iter()
                    .enumerate()
                    .map(|(index, value)| {
                        self.decode(element, value)
                            .map(ValidatedValue::into_value)
                            .map_err(|source| ValueCodecError::Element {
                                index,
                                source: Box::new(source),
                            })
                    })
                    .collect::<Result<_, _>>()?,
            )
        } else if let Some(identity) = &contract.identity {
            match wire {
                Value::Object(mut fields) if fields.len() == identity.len() => {
                    let mut result = indexmap::IndexMap::new();
                    for (key, member) in identity {
                        let value = fields.shift_remove(key).ok_or(
                            crate::entity_ref_value::EntityRefValueError::KeyShapeMismatch,
                        )?;
                        result.insert(
                            key.clone(),
                            self.decode(member, value)
                                .map_err(|source| ValueCodecError::Field {
                                    field: key.clone(),
                                    source: Box::new(source),
                                })?
                                .into_value(),
                        );
                    }
                    Value::Object(result)
                }
                value if identity.len() == 1 => self.decode(&identity[0].1, value)?.into_value(),
                _ => {
                    return Err(
                        crate::entity_ref_value::EntityRefValueError::KeyShapeMismatch.into(),
                    )
                }
            }
        } else if matches!(contract.schema.field_type, FieldType::Money) {
            let format = match contract.schema.value_format {
                Some(crate::ValueWireFormat::Money(format)) => format,
                _ => crate::money::MoneyWireFormat::DecimalString,
            };
            crate::money::normalize_concrete(wire, format, contract.schema.currency.as_deref())
                .map_err(crate::CoercionError::Money)?
        } else {
            decode_shape(
                &contract.schema.field_type,
                contract.schema.value_format,
                contract.schema.array_items.as_ref(),
                wire,
                contract.nullable,
            )?
        };
        crate::capability_input::validate_concrete_named_value_domain(&value, &contract.schema)?;
        if let Value::Money(money) = &value {
            crate::money::currency_conflict(contract.schema.currency.as_deref(), money.currency())?;
        }
        Ok(ValidatedValue {
            contract: contract.clone(),
            value,
        })
    }
    fn encode(
        &self,
        contract: &ResolvedValueContract,
        value: &ValidatedValue,
    ) -> Result<Value, ValueCodecError> {
        if contract != &value.contract {
            return Err(ValueCodecError::ContractMismatch);
        }
        Ok(value.value.clone())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct JsonValueCodec;

/// Internal persistence retains semantic values rather than outbound scalar profiles.
#[derive(Debug, Clone, Copy)]
pub struct StorageValueCodec;

impl ValueCodec for StorageValueCodec {
    type Wire = serde_json::Value;
    fn decode(
        &self,
        contract: &ResolvedValueContract,
        wire: Self::Wire,
    ) -> Result<ValidatedValue, ValueCodecError> {
        ValueWireCodec.decode(contract, crate::try_storage_json_to_plasm_value(&wire)?)
    }
    fn encode(
        &self,
        contract: &ResolvedValueContract,
        value: &ValidatedValue,
    ) -> Result<Self::Wire, ValueCodecError> {
        let wire =
            crate::try_plasm_value_to_storage_json(&ValueWireCodec.encode(contract, value)?)?;
        if self.decode(contract, wire.clone())? != *value {
            return Err(ValueCodecError::LossyWireRepresentation);
        }
        Ok(wire)
    }
}

/// Textual structural identity slots have an explicit, schema-directed wire format.
/// Parsing here never changes an ordinary provider string into a number.
#[derive(Debug, Clone, Copy)]
pub struct IdentitySlotCodec;

impl ValueCodec for IdentitySlotCodec {
    type Wire = String;
    fn decode(
        &self,
        contract: &ResolvedValueContract,
        wire: String,
    ) -> Result<ValidatedValue, ValueCodecError> {
        let invalid = || ValueCodecError::IdentitySlot {
            expected: contract.schema.field_type.clone(),
        };
        let value = match contract.schema.field_type {
            FieldType::Integer => Value::Integer(wire.parse().map_err(|_| invalid())?),
            FieldType::Number => {
                let json: serde_json::Number =
                    serde_json::from_str(&wire).map_err(|_| invalid())?;
                crate::json_value_to_plasm_value(&serde_json::Value::Number(json))
            }
            FieldType::Boolean => Value::Bool(wire.parse().map_err(|_| invalid())?),
            _ => Value::String(wire),
        };
        ValueWireCodec.decode(contract, value)
    }
    fn encode(
        &self,
        contract: &ResolvedValueContract,
        value: &ValidatedValue,
    ) -> Result<String, ValueCodecError> {
        let wire = match ValueWireCodec.encode(contract, value)? {
            Value::String(value) => Ok(value),
            value @ (Value::Integer(_) | Value::Unsigned(_) | Value::Float(_) | Value::Bool(_)) => {
                Ok(crate::try_plasm_value_to_json(&value)?.to_string())
            }
            _ => Err(ValueCodecError::IdentitySlot {
                expected: contract.schema.field_type.clone(),
            }),
        }?;
        if self.decode(contract, wire.clone())? != *value {
            return Err(ValueCodecError::LossyWireRepresentation);
        }
        Ok(wire)
    }
}

impl ValueCodec for JsonValueCodec {
    type Wire = serde_json::Value;
    fn decode(
        &self,
        contract: &ResolvedValueContract,
        wire: Self::Wire,
    ) -> Result<ValidatedValue, ValueCodecError> {
        fn lift(contract: &ResolvedValueContract, wire: &serde_json::Value) -> Value {
            if let (Some(element), serde_json::Value::Array(values)) = (&contract.element, wire) {
                return Value::Array(values.iter().map(|value| lift(element, value)).collect());
            }
            if matches!(contract.schema.field_type, FieldType::Money) {
                crate::money::json_amount_to_value(wire)
            } else {
                crate::json_value_to_plasm_value(wire)
            }
        }
        ValueWireCodec.decode(contract, lift(contract, &wire))
    }
    fn encode(
        &self,
        contract: &ResolvedValueContract,
        value: &ValidatedValue,
    ) -> Result<Self::Wire, ValueCodecError> {
        let wire = crate::try_plasm_value_to_json(&ValueWireCodec.encode(contract, value)?)?;
        let restored = self.decode(contract, wire.clone())?;
        if restored != *value {
            return Err(ValueCodecError::LossyWireRepresentation);
        }
        Ok(wire)
    }
}

fn validate_concrete_tree(value: &Value) -> Result<(), ValueCodecError> {
    match value {
        Value::Array(values) => values.iter().enumerate().try_for_each(|(index, value)| {
            validate_concrete_tree(value).map_err(|source| ValueCodecError::Element {
                index,
                source: Box::new(source),
            })
        }),
        Value::Object(fields) => fields.iter().try_for_each(|(field, value)| {
            validate_concrete_tree(value).map_err(|source| ValueCodecError::Field {
                field: field.clone(),
                source: Box::new(source),
            })
        }),
        value => {
            crate::try_plasm_value_to_storage_json(value)?;
            crate::operand_binding::ResolvedValue::new(value.clone())
                .map_err(|source| ValueCodecError::ConcreteValue(Box::new(source)))?;
            Ok(())
        }
    }
}

fn decode_shape(
    ft: &FieldType,
    format: Option<crate::ValueWireFormat>,
    items: Option<&crate::ArrayItemsSchema>,
    value: Value,
    nullable: bool,
) -> Result<Value, ValueCodecError> {
    if matches!(value, Value::Null) {
        return if nullable {
            Ok(value)
        } else {
            Err(ValueCodecError::Null)
        };
    }
    let invalid = || ValueCodecError::Shape {
        expected: ft.to_string(),
        actual: value.type_name(),
    };
    match ft {
        FieldType::Boolean if matches!(value, Value::Bool(_)) => Ok(value),
        FieldType::Integer if matches!(value, Value::Integer(_)) => Ok(value),
        // JSON numbers retain their exact representation; no i64 -> f64 rounding.
        FieldType::Number
            if matches!(
                value,
                Value::Integer(_) | Value::Unsigned(_) | Value::Float(_)
            ) =>
        {
            Ok(value)
        }
        FieldType::String | FieldType::Uuid | FieldType::Select
            if matches!(value, Value::String(_)) =>
        {
            Ok(value)
        }
        FieldType::EntityRef { .. } => {
            Ok(crate::entity_ref_value::EntityRefPayload::try_from_value(&value)?.to_value())
        }
        FieldType::Array | FieldType::MultiSelect => {
            let Value::Array(values) = value else {
                return Err(invalid());
            };
            values
                .into_iter()
                .enumerate()
                .map(|(index, value)| {
                    match items {
                        Some(item) => {
                            decode_shape(&item.field_type, item.value_format, None, value, false)
                        }
                        None if matches!(ft, FieldType::MultiSelect) => {
                            decode_shape(&FieldType::String, None, None, value, false)
                        }
                        None => Ok(value),
                    }
                    .map_err(|source| ValueCodecError::Element {
                        index,
                        source: Box::new(source),
                    })
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array)
        }
        FieldType::Json | FieldType::Blob => Ok(value),
        // These are declared representation profiles, not generic scalar coercions.
        FieldType::Date => match format {
            Some(crate::ValueWireFormat::Temporal(format)) => {
                Ok(crate::temporal::normalize_temporal_value(value, format)
                    .map_err(crate::CoercionError::Temporal)?)
            }
            _ => Err(crate::CoercionError::MissingTemporalFormat.into()),
        },
        FieldType::DigitId => {
            Ok(crate::wire_coercion::coerce_digit_id(value)
                .map_err(crate::CoercionError::DigitId)?)
        }
        _ => Err(invalid()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value_domain::{KernelKind, ValueDomain};
    fn key(value: &str) -> crate::ValueDomainKey {
        crate::ValueDomainKey::new(value).unwrap()
    }

    #[test]
    fn catalog_identity_is_preserved_across_provider_storage_and_observation() {
        let mut cgs = crate::loader::load_schema_dir(
            &std::path::Path::new(crate::test_manifest_dir())
                .join("../../fixtures/schemas/python_dag_slice"),
        )
        .unwrap();
        cgs.values
            .insert("item_id".into(), contract(KernelKind::Integer).schema);
        let reference_key = cgs.entities["Tag"].fields["item_id"]
            .kind
            .registry_key()
            .clone();
        let reference = ResolvedValueContract::resolve(&cgs, "", &reference_key, false).unwrap();
        let number = ValueWireCodec
            .decode(&reference, Value::Integer(9_007_199_254_740_993))
            .unwrap();
        assert_eq!(
            JsonValueCodec
                .decode(
                    &reference,
                    JsonValueCodec.encode(&reference, &number).unwrap()
                )
                .unwrap(),
            number
        );
        for invalid in [Value::String("2".into()), Value::Bool(true), Value::Null] {
            assert!(ValueWireCodec.decode(&reference, invalid).is_err());
        }
        let identity = crate::Ref::new("Item", "9007199254740993");
        let row_codec = crate::row_contract::RowCodec::new(Some(&cgs));
        let row = row_codec.identity_values(&identity).unwrap();
        assert_eq!(row.get("id"), Some(&Value::Integer(9_007_199_254_740_993)));
        let observed = crate::entity_ref_value::observed_reference_payload(
            &row.into_value(),
            &cgs.entities["Item"],
            "Item",
            &cgs,
        )
        .unwrap();
        assert_eq!(observed, number.into_value());
        assert!(row_codec
            .identity_values(&crate::Ref::new("Item", "invalid"))
            .is_err());
        let mut compound = cgs.entities["Item"].clone();
        compound.key_vars = vec!["id".into(), "title".into()];
        cgs.entities.insert("Item".into(), compound);
        let identity = crate::Ref::compound(
            "Item",
            [("id".into(), "2".into()), ("title".into(), "0002".into())].into(),
        );
        let row = crate::row_contract::RowCodec::new(Some(&cgs))
            .identity_values(&identity)
            .unwrap();
        assert_eq!(row.get("id"), Some(&Value::Integer(2)));
        assert_eq!(row.get("title"), Some(&Value::String("0002".into())));
    }

    #[test]
    fn nested_elements_keep_domain_constraints_and_catalog_pins() {
        let mut cgs = crate::CGS::default();
        let email = NamedValueSchema::from_domain(
            String::new(),
            ValueDomain::new(
                KernelKind::String,
                Some(crate::value_domain::ProfileId::Email),
                Default::default(),
                None,
                None,
            )
            .unwrap(),
            None,
        );
        cgs.values.insert("email".into(), email);
        for (name, child) in [("emails", "email"), ("groups", "emails")] {
            let child_schema = &cgs.values[child];
            let items = crate::ArrayItemsSchema {
                kind: crate::FieldValueKind::Registry(key(child)),
                field_type: child_schema.field_type.clone(),
                value_format: child_schema.value_format,
                allowed_values: None,
            };
            cgs.values.insert(
                name.into(),
                NamedValueSchema::from_domain(
                    String::new(),
                    ValueDomain::new(KernelKind::Array, None, Default::default(), None, None)
                        .unwrap(),
                    Some(items),
                ),
            );
        }
        let contract = ResolvedValueContract::resolve(&cgs, "", &key("groups"), false).unwrap();
        let valid = serde_json::json!([["reader@example.test"]]);
        let decoded = JsonValueCodec.decode(&contract, valid.clone()).unwrap();
        assert_eq!(JsonValueCodec.encode(&contract, &decoded).unwrap(), valid);
        assert!(
            matches!(JsonValueCodec.decode(&contract, serde_json::json!([["bad"]])), Err(ValueCodecError::Element { index: 0, source }) if matches!(*source, ValueCodecError::Element { index: 0, .. }))
        );
        cgs.values.get_mut("email").unwrap().description = "changed catalog".into();
        let different = ResolvedValueContract::resolve(&cgs, "", &key("groups"), false).unwrap();
        assert_eq!(
            JsonValueCodec.encode(&different, &decoded),
            Err(ValueCodecError::ContractMismatch)
        );
    }

    #[test]
    fn declared_profiles_and_structural_money_roundtrip() {
        use crate::value_domain::ProfileId;
        for (kernel, profile, currency, wire) in [
            (
                KernelKind::String,
                Some(ProfileId::DigitId),
                None,
                serde_json::json!("000123"),
            ),
            (
                KernelKind::String,
                Some(ProfileId::Iso8601Date),
                None,
                serde_json::json!("2026-10-17"),
            ),
            (
                KernelKind::Money,
                None,
                Some("USD".to_owned()),
                serde_json::json!("12.34"),
            ),
        ] {
            let schema = NamedValueSchema::from_domain(
                String::new(),
                ValueDomain::new(kernel, profile, Default::default(), None, currency).unwrap(),
                None,
            );
            let contract = ResolvedValueContract::new(&schema, false).unwrap();
            let decoded = JsonValueCodec.decode(&contract, wire).unwrap();
            assert_eq!(
                JsonValueCodec
                    .decode(
                        &contract,
                        JsonValueCodec.encode(&contract, &decoded).unwrap()
                    )
                    .unwrap(),
                decoded
            );
            let stored = crate::try_plasm_value_to_storage_json(decoded.value()).unwrap();
            assert_eq!(
                crate::try_storage_json_to_plasm_value(&stored).unwrap(),
                *decoded.value()
            );
        }
    }
    fn contract(kernel: KernelKind) -> ResolvedValueContract {
        let domain = ValueDomain::new(kernel, None, Default::default(), None, None).unwrap();
        ResolvedValueContract::new(
            &NamedValueSchema::from_domain(String::new(), domain, None),
            false,
        )
        .unwrap()
    }
    #[test]
    fn exact_numeric_and_reference_roundtrips() {
        let reference = contract(KernelKind::EntityRef {
            entry_id: Default::default(),
            target: "Parent".into(),
        });
        for value in [
            Value::Integer(2),
            Value::Integer(9_007_199_254_740_993),
            Value::Integer(i64::MAX),
            Value::Unsigned(u64::MAX),
            Value::Float(1e20),
            Value::Bool(true),
            Value::String("0002".into()),
        ] {
            let decoded = ValueWireCodec.decode(&reference, value.clone()).unwrap();
            assert_eq!(decoded.value(), &value);
            let wire = JsonValueCodec.encode(&reference, &decoded).unwrap();
            assert_eq!(JsonValueCodec.decode(&reference, wire).unwrap(), decoded);
        }
        let number = contract(KernelKind::Number);
        let value = Value::Integer(9_007_199_254_740_993);
        assert_eq!(
            ValueWireCodec
                .decode(&number, value.clone())
                .unwrap()
                .into_value(),
            value
        );
    }
    #[test]
    fn invalid_wire_values_never_become_absence_or_different_shapes() {
        for number in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(crate::try_plasm_value_to_json(&Value::Float(number)).is_err());
            assert!(ValueWireCodec
                .decode(&contract(KernelKind::Number), Value::Float(number))
                .is_err());
        }
        assert!(ValueWireCodec
            .decode(&contract(KernelKind::Number), Value::String("NaN".into()))
            .is_err());
        assert!(ValueWireCodec
            .decode(&contract(KernelKind::Array), Value::Integer(7))
            .is_err());
        assert!(ValueWireCodec
            .decode(&contract(KernelKind::String), Value::Integer(7))
            .is_err());
        assert!(ValueWireCodec
            .decode(&contract(KernelKind::Integer), Value::Null)
            .is_err());
        assert!(
            crate::try_plasm_value_to_json(&Value::Array(vec![Value::Float(f64::NAN)])).is_err()
        );
    }
    #[test]
    fn validated_values_cannot_change_contracts() {
        let integer = contract(KernelKind::Integer);
        let number = contract(KernelKind::Number);
        let value = ValueWireCodec.decode(&integer, Value::Integer(3)).unwrap();
        assert_eq!(
            ValueWireCodec.encode(&number, &value),
            Err(ValueCodecError::ContractMismatch)
        );
        let mut bad = integer.schema.clone();
        bad.field_type = FieldType::String;
        assert_eq!(
            ResolvedValueContract::new(&bad, false),
            Err(ValueCodecError::InconsistentContract)
        );
    }
    #[test]
    fn provider_objects_do_not_interpret_storage_tags() {
        let contract = contract(KernelKind::Json);
        let wire =
            serde_json::json!({"nested": [{"__plasm_money":"12", "currency":7, "extra":"retain"}]});
        let value = JsonValueCodec.decode(&contract, wire.clone()).unwrap();
        assert_eq!(JsonValueCodec.encode(&contract, &value).unwrap(), wire);
    }

    #[test]
    fn concrete_profiles_reject_placeholders_and_invalid_currency() {
        let money = contract(KernelKind::Money);
        assert!(JsonValueCodec
            .decode(&money, serde_json::json!("$"))
            .is_err());
        assert!(matches!(
            JsonValueCodec.decode(&money, serde_json::json!({"amount":"12", "currency":7})),
            Err(ValueCodecError::Coercion(crate::CoercionError::Money(
                crate::MoneyError::CurrencyFieldNotString { .. }
            )))
        ));
        let unstamped = Value::Money(crate::MoneyValue::new("12.34".parse().unwrap(), None));
        assert!(ValueWireCodec.decode(&money, unstamped).is_ok());
        let schema = NamedValueSchema::from_domain(
            String::new(),
            ValueDomain::new(
                KernelKind::String,
                Some(crate::value_domain::ProfileId::Email),
                Default::default(),
                None,
                None,
            )
            .unwrap(),
            None,
        );
        let email = ResolvedValueContract::new(&schema, false).unwrap();
        assert!(JsonValueCodec
            .decode(&email, serde_json::json!("$"))
            .is_err());
    }

    #[test]
    fn identity_wire_rejects_representation_loss() {
        let number = contract(KernelKind::Number);
        let value = ValueWireCodec.decode(&number, Value::Unsigned(2)).unwrap();
        assert_eq!(
            IdentitySlotCodec.encode(&number, &value),
            Err(ValueCodecError::LossyWireRepresentation)
        );
    }

    #[test]
    fn storage_preserves_reserved_objects_and_typed_values() {
        let raw = crate::json_value_to_plasm_value(&serde_json::json!({"nested": [
            {"__plasm_money":"12", "currency":7, "extra":"keep"},
            {"__plasm_object":{"__plasm_unsigned":"2"}}
        ]}));
        for value in [
            raw,
            Value::Unsigned(2),
            Value::Money(crate::MoneyValue::new(
                "12.34".parse().unwrap(),
                Some("USD".into()),
            )),
        ] {
            let wire = crate::try_plasm_value_to_storage_json(&value).unwrap();
            assert_eq!(
                crate::try_storage_json_to_plasm_value(&wire).unwrap(),
                value
            );
            let resolved = crate::operand_binding::ResolvedValue::new(value).unwrap();
            assert_eq!(
                crate::operand_binding::ResolvedValue::from_wire(resolved.to_wire().unwrap())
                    .unwrap(),
                resolved
            );
        }
        for wire in [
            serde_json::json!({"__plasm_money":"12", "currency":7}),
            serde_json::json!({"__plasm_money":"12", "format":"invalid"}),
        ] {
            assert_eq!(
                crate::try_storage_json_to_plasm_value(&wire),
                Err(ValueCodecError::InvalidStorageTag)
            );
        }
    }

    #[test]
    fn money_decimal_and_currency_contracts_are_exact() {
        let money = contract(KernelKind::Money);
        let value = JsonValueCodec
            .decode(&money, serde_json::json!(12.34))
            .unwrap();
        let Value::Money(amount) = value.value() else {
            panic!("money")
        };
        assert_eq!(amount.amount().to_string(), "12.34");
        let schema = NamedValueSchema::from_domain(
            String::new(),
            ValueDomain::new(
                KernelKind::Money,
                None,
                Default::default(),
                None,
                Some("USD".into()),
            )
            .unwrap(),
            None,
        );
        let usd = ResolvedValueContract::new(&schema, false).unwrap();
        assert!(matches!(
            JsonValueCodec.decode(&usd, serde_json::json!({"amount":12.34, "currency":"EUR"})),
            Err(ValueCodecError::Currency(_))
        ));
        for amount in [1e20, -1e20, -(i64::MIN as f64)] {
            assert!(matches!(
                crate::money::normalize_concrete(
                    Value::Float(amount),
                    crate::money::MoneyWireFormat::MinorUnits { scale: 2 },
                    None
                ),
                Err(crate::MoneyError::MinorUnitsOverflow { .. })
            ));
        }
    }

    #[test]
    fn nested_invalid_values_retain_their_location() {
        let invalid = Value::Object(indexmap::IndexMap::from([(
            "items".into(),
            Value::Array(vec![Value::Float(f64::NAN)]),
        )]));
        let error = ValueWireCodec
            .decode(&contract(KernelKind::Json), invalid)
            .unwrap_err();
        let ValueCodecError::Field { field, source } = error else {
            panic!("field context")
        };
        assert_eq!(field, "items");
        let ValueCodecError::Element { index, source } = *source else {
            panic!("element context")
        };
        assert_eq!(index, 0);
        assert_eq!(
            *source,
            ValueCodecError::Encoding(crate::WireEncodingError::NonFinite)
        );
    }
    proptest::proptest! {
        #[test]
        fn integer_json_roundtrip(value: i64) {
            let c = contract(KernelKind::Integer);
            let v = ValueWireCodec.decode(&c, Value::Integer(value)).unwrap();
            proptest::prop_assert_eq!(JsonValueCodec.decode(&c, JsonValueCodec.encode(&c, &v).unwrap()).unwrap(), v);
        }
    }
}
