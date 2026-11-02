use super::walk::{record, variants, Structural, Walk};
use crate::money::MoneyWireFormat;
use crate::schema::{
    EmbedOnMissPolicy, EmbeddedCollectionCoverage, JsonPathSegment, RelationMaterialization,
    RelationScopedFallback,
};
use crate::temporal_value::TemporalKind;
use crate::value::{CompOp, FieldType, GetScalarExtract, PlasmInputRef, TemporalWireFormat, Value};
use crate::{RelationBindingProof, ValidatedViewEmbedProof};

macro_rules! atom {
    ($($ty:ty),* $(,)?) => {$(impl Structural for $ty {
        fn visit<'a>(&'a self, w: &mut Walk<'a, '_>) { w.bytes(self.as_str().as_bytes()); }
    })*};
}
atom!(
    crate::EntityName,
    crate::EntityId,
    crate::CapabilityName,
    crate::RegistryEntryId,
    crate::CapabilityParamName,
    crate::EntityFieldName,
    crate::DataClassName,
    crate::ValueDomainKey,
    crate::plasm_monad::BindingName,
    crate::plasm_monad::OutputName,
    crate::PagingHandle,
    crate::OperationHandle
);
impl Structural for crate::catalog_id::CatalogEntryStamp {
    fn visit<'a>(&'a self, w: &mut Walk<'a, '_>) {
        w.fields("CatalogEntryStamp", &[&self.0]);
    }
}
impl Structural for crate::plasm_monad::FieldPath {
    fn visit<'a>(&'a self, w: &mut Walk<'a, '_>) {
        w.fields(
            "FieldPath",
            &self
                .segments()
                .iter()
                .map(|s| s as &dyn Structural)
                .collect::<Vec<_>>(),
        );
    }
}
variants!(PlasmInputRef { NodeInput { node, path }, RowBinding { binding, path } });
record!(GetScalarExtract {
    entity,
    identity,
    wire,
    catalog_entry_id
});
// Positive signed and unsigned operands have the same runtime numeric meaning and
// persistence may choose either representation. Floats remain a separate category.
impl Structural for Value {
    fn visit<'a>(&'a self, w: &mut Walk<'a, '_>) {
        match self {
            Self::Null => w.fields("Value::Null", &[]),
            Self::Bool(v) => w.fields("Value::Bool", &[v]),
            Self::Integer(v) if *v >= 0 => {
                w.bytes(b"Value::Natural");
                w.nested(&(*v as u64));
            }
            Self::Integer(v) => w.fields("Value::Negative", &[v]),
            Self::Unsigned(v) => {
                w.bytes(b"Value::Natural");
                w.nested(v);
            }
            Self::Float(v) => w.fields("Value::Float", &[v]),
            Self::String(v) => w.fields("Value::String", &[v]),
            Self::PhraseIdent(v) => w.fields("Value::PhraseIdent", &[v]),
            Self::Array(v) => w.fields("Value::Array", &[v]),
            Self::Object(v) => w.fields("Value::Object", &[v]),
            Self::Money(v) => w.fields("Value::Money", &[v]),
            Self::PlasmInputRef(v) => w.fields("Value::InputRef", &[v]),
            Self::GetScalarExtract(v) => w.fields("Value::GetScalarExtract", &[v]),
            Self::StringTemplate(v) => w.fields("Value::StringTemplate", &[v]),
            Self::UnionCtor {
                ctor_label,
                ctor_fields,
            } => w.fields("Value::UnionCtor", &[ctor_label, ctor_fields]),
        }
    }
}
impl Structural for crate::typed_invoke::InvokeInputPayload {
    fn visit<'a>(&'a self, w: &mut Walk<'a, '_>) {
        // These are two internal representations of the same lowered operand. Use
        // typed lowering, never Serialize, to remove that storage distinction.
        match self {
            Self::Raw(value) => value.visit(w),
            Self::Typed(value) => w.nested(&value.to_value()),
        }
    }
}
impl Structural for crate::typed_literal::TypedComparisonValue {
    fn visit<'a>(&'a self, w: &mut Walk<'a, '_>) {
        w.nested(&self.to_value());
    }
}
impl Structural for crate::operand_binding::ResolvedValue {
    fn visit<'a>(&'a self, w: &mut Walk<'a, '_>) {
        self.value().visit(w);
    }
}
impl Structural for crate::program_string_template::CompiledProgramString {
    fn visit<'a>(&'a self, w: &mut Walk<'a, '_>) {
        // The compiled environment and dependency paths are derived from source.
        w.bytes(self.source().as_bytes());
    }
}
impl Structural for crate::money::MoneyValue {
    fn visit<'a>(&'a self, w: &mut Walk<'a, '_>) {
        let amount = self.amount();
        w.bytes(&amount.mantissa().to_le_bytes());
        w.nested(&amount.scale());
        w.nested(&self.currency().map(str::to_owned));
        w.nested(&self.stored_format());
    }
}
variants!(MoneyWireFormat { DecimalString, JsonNumber, MinorUnits { scale } });
variants!(TemporalKind {
    Date,
    Datetime,
    Time,
    Timedelta,
    Timezone
});
variants!(TemporalWireFormat {
    Rfc3339,
    Iso8601NaiveDatetime,
    UnixMs,
    UnixSec,
    Iso8601Date
});
variants!(FieldType { Boolean, Number, Integer, Uuid, DigitId, Blob, String, Select, MultiSelect, Date, Array, Json, Money, EntityRef { entry_id, target } });
variants!(CompOp {
    Eq,
    Neq,
    Gt,
    Lt,
    Gte,
    Lte,
    In,
    Contains,
    Exists
});
record!(RelationBindingProof {
    cap_param,
    parent_field
});
record!(ValidatedViewEmbedProof {
    view,
    producer_node,
    relation
});
variants!(JsonPathSegment { Key { key }, Wildcard { wildcard } });
variants!(EmbeddedCollectionCoverage { Unknown, Complete });
variants!(EmbedOnMissPolicy { FallbackScoped });
variants!(RelationScopedFallback {
    QueryScoped { capability, param }, QueryScopedBindings { capability, bindings },
    HydrateFromEmbedPath { path, get_capability }
});
variants!(RelationMaterialization {
    Unavailable, FromParentGet { path, collection_coverage },
    PreferFromParentGet { path, collection_coverage, on_embed_miss, fallback },
    QueryScoped { capability, param, select_entity },
    QueryScopedBindings { capability, bindings, select_entity },
    GetScopedBindings { capability, bindings }, ViewEmbed { view }
});

// Metadata is itself JSON data, not a serialization of the program. Traverse its
// value algebra with the same framing and unordered object semantics.
impl Structural for serde_json::Value {
    fn visit<'a>(&'a self, w: &mut Walk<'a, '_>) {
        match self {
            Self::Null => w.fields("Json::Null", &[]),
            Self::Bool(v) => w.fields("Json::Bool", &[v]),
            Self::String(v) => w.fields("Json::String", &[v]),
            Self::Array(v) => w.fields("Json::Array", &[v]),
            Self::Object(v) => {
                let sorted: std::collections::BTreeMap<_, _> = v.iter().collect();
                let fields: Vec<&dyn Structural> = sorted
                    .into_iter()
                    .flat_map(|(k, v)| [k as &dyn Structural, v as &dyn Structural])
                    .collect();
                w.fields("Json::Object", &fields);
            }
            Self::Number(v) => {
                if let Some(value) = v.as_u64() {
                    w.bytes(b"Json::Natural");
                    w.nested(&value);
                } else if let Some(value) = v.as_i64() {
                    w.bytes(b"Json::Negative");
                    w.nested(&value);
                } else {
                    w.bytes(b"Json::Float");
                    w.nested(&v.as_f64());
                }
            }
        }
    }
}
