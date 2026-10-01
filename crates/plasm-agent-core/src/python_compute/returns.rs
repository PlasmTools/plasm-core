//! Return annotations describe values, never runtime entity authority.
use super::*;
use plasm_core::value_contract::{ValueContract as Type, ValueShape};

pub(super) fn resolve(
    expr: &Expr,
    input: &Type,
    domains: &BTreeMap<String, Type>,
    catalogs: &BTreeMap<String, std::sync::Arc<CGS>>,
    cgs: &CGS,
    entry: &str,
    symbols: &dyn SymbolResolve,
) -> Result<Type, String> {
    Resolver {
        input,
        domains,
        catalogs,
        cgs,
        entry,
        symbols,
    }
    .resolve(expr, 0)
}

struct Resolver<'a> {
    input: &'a Type,
    domains: &'a BTreeMap<String, Type>,
    catalogs: &'a BTreeMap<String, std::sync::Arc<CGS>>,
    cgs: &'a CGS,
    entry: &'a str,
    symbols: &'a dyn SymbolResolve,
}
impl Resolver<'_> {
    fn resolve(&self, expr: &Expr, depth: usize) -> Result<Type, String> {
        let Self {
            input,
            domains,
            catalogs,
            cgs,
            entry,
            symbols,
        } = *self;
        if depth >= 64 {
            return Err("return annotation depth exceeded".into());
        }
        let recur = |e| self.resolve(e, depth + 1);
        Ok(match expr {
            Expr::Name(n) => match n.id.as_str() {
                name if plasm_core::temporal_value::TemporalKind::parse(name).is_some() => plasm_core::temporal_value::TemporalKind::parse(name).unwrap().contract(),
                "Row" => input.clone(),
                "dict" => return Err("dict has no declared field contract; omit the return annotation on a single-return compute to infer its structural record, or use Row/Value[eN] for an existing contract".into()),
                "bool" => Type::scalar(FieldType::Boolean),
                "int" => Type::scalar(FieldType::Integer),
                "float" => Type::scalar(FieldType::Number),
                "str" => Type::scalar(FieldType::String),
                symbol => {
                    let t = domains.get(symbol).ok_or_else(|| format!("unknown Plasm return type {symbol}"))?;
                    t.clone()
                }
            },
            Expr::NoneLiteral(_) => Type { shape: ValueShape::Null, domain: None, nullable: true },
            Expr::BinOp(b) if b.op == ruff_python_ast::Operator::BitOr => {
                let left = recur(&b.left)?;
                let right = recur(&b.right)?;
                Type::join(left, right)
            }
            Expr::Attribute(a) => {
                let base = recur(&a.value)?;
                let fields = match &base.shape {
                    ValueShape::Record { fields } | ValueShape::ObservedRecord { fields, .. } => fields,
                    _ => return Err("return type field requires a record contract".into()),
                };
                fields.get(a.attr.as_str()).cloned().ok_or("unknown return type field")?
            }
            Expr::Subscript(s) if name(&s.value) == Some("list") => Type { shape: ValueShape::Array { element: Box::new(recur(&s.slice)?) }, domain: None, nullable: false },
            Expr::Subscript(s) if name(&s.value).is_some_and(is_entity_record_type) => {
                let token = name(&s.slice).ok_or("expected entity symbol")?;
                let owner = symbols.resolve_session_entity(token).map_err(|e| e.to_string())?;
                let (cgs, entry) = if owner.entry_id.as_str() == entry { (cgs, entry) } else {
                    (catalogs.get(owner.entry_id.as_str()).map(AsRef::as_ref).ok_or("return entity catalog is not loaded")?, owner.entry_id.as_str())
                };
                let value = ValueContract::from_cgs(cgs, entry, symbols, token)?;
                Type::record(value.fields.into_iter().filter(|(_, f)| f.domain.is_some()).map(|(k,f)| (k, f.value_type)).collect(), Default::default())
            }
            _ => return Err("unsupported Plasm return annotation; use a primitive, v#, Row, Value[e#], list[T], T | U, or a record type field".into()),
        })
    }
}
