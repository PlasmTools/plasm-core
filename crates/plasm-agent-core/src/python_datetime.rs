//! Static resolution of the supported standard-library namespace, not Python execution.
use ruff_python_ast::{Expr, Stmt};
use ruff_text_size::Ranged;
use std::collections::BTreeMap;

pub const PRELUDE: &str = "from datetime import date, datetime, time, timedelta, timezone\n";

#[derive(Default, Clone)]
pub(crate) struct Imports {
    pub bindings: BTreeMap<String, String>,
    pub source: String,
}
impl Imports {
    pub fn add(&mut self, stmt: &Stmt, source: &str) -> Result<bool, String> {
        let bindings = match stmt {
            Stmt::Import(i) => i
                .names
                .iter()
                .map(|a| {
                    if a.name.as_str() != "datetime" {
                        return Err("only the datetime module is admitted".to_string());
                    }
                    Ok((
                        a.asname.as_ref().unwrap_or(&a.name).to_string(),
                        "datetime".to_string(),
                    ))
                })
                .collect::<Result<Vec<_>, _>>()?,
            Stmt::ImportFrom(i) => {
                if i.level != 0 || i.module.as_ref().map(|m| m.as_str()) != Some("datetime") {
                    return Err("only the datetime module is admitted".into());
                }
                i.names
                    .iter()
                    .map(|a| {
                        if plasm_core::temporal_value::TemporalKind::parse(a.name.as_str())
                            .is_none()
                        {
                            return Err(format!(
                                "Monty does not expose datetime.{}; use timezone.utc for UTC",
                                a.name
                            ));
                        }
                        Ok((
                            a.asname.as_ref().unwrap_or(&a.name).to_string(),
                            format!("datetime.{}", a.name),
                        ))
                    })
                    .collect::<Result<Vec<_>, String>>()?
            }
            _ => return Ok(false),
        };
        for (local, canonical) in bindings {
            if local.starts_with('_')
                || matches!(
                    local.as_str(),
                    "Program"
                        | "compute"
                        | "Value"
                        | "Row"
                        | "self"
                        | "agg"
                        | "str"
                        | "int"
                        | "float"
                        | "bool"
                        | "list"
                        | "len"
                        | "any"
                        | "all"
                )
                || (local
                    .as_bytes()
                    .first()
                    .is_some_and(|c| b"emrvp".contains(c))
                    && local.len() > 1
                    && local.as_bytes()[1..].iter().all(u8::is_ascii_digit))
                || self.bindings.insert(local, canonical).is_some()
            {
                return Err("reserved or duplicate import binding".into());
            }
        }
        self.source.push_str(&source[stmt.range()]);
        self.source.push('\n');
        Ok(true)
    }
    pub fn path(&self, e: &Expr) -> Option<String> {
        match e {
            Expr::Name(n) => self.bindings.get(n.id.as_str()).cloned(),
            Expr::Attribute(a) => self.path(&a.value).map(|base| format!("{base}.{}", a.attr)),
            _ => None,
        }
    }
    pub fn split<'a>(source: &str, suite: &'a [Stmt]) -> Result<(Self, &'a [Stmt]), String> {
        let mut imports = Self::default();
        let mut n = 0;
        for stmt in suite {
            if !imports.add(stmt, source)? {
                break;
            }
            n += 1;
        }
        Ok((imports, &suite[n..]))
    }
}
