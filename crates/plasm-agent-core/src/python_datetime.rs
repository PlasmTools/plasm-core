//! Host alias bookkeeping only. Monty owns import and member availability.
use ruff_python_ast::Stmt;
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
                    let name = a.name.as_str();
                    match &a.asname {
                        Some(alias) => (alias.to_string(), name.to_owned()),
                        None => {
                            let root = name.split('.').next().unwrap_or(name);
                            (root.to_owned(), root.to_owned())
                        }
                    }
                })
                .collect::<Vec<_>>(),
            Stmt::ImportFrom(i) => i
                .names
                .iter()
                .filter(|a| a.name.as_str() != "*")
                .map(|a| {
                    (
                        a.asname.as_ref().unwrap_or(&a.name).to_string(),
                        format!(
                            "{}.{}",
                            i.module.as_ref().map(|m| m.as_str()).unwrap_or(""),
                            a.name
                        ),
                    )
                })
                .collect(),
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
            {
                return Err("import shadows a reserved Plasm binding".into());
            }
            self.bindings.insert(local, canonical);
        }
        self.source.push_str(&source[stmt.range()]);
        self.source.push('\n');
        Ok(true)
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
