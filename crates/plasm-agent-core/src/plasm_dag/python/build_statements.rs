//! Closed outer-build statement vocabulary; inner Python remains Monty's language.
use super::*;

macro_rules! build_statements {
    ($($variant:ident($payload:ty) => $name:literal),+ $(,)?) => {
        pub(super) enum BuildStatement<'a> { $($variant($payload)),+ }
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum BuildStatementKind { $($variant),+ }
        impl BuildStatementKind {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            pub fn name(self) -> &'static str { match self { $(Self::$variant => $name),+ } }
        }
        impl BuildStatement<'_> {
            fn kind(&self) -> BuildStatementKind {
                match self { $(Self::$variant(_) => BuildStatementKind::$variant),+ }
            }
        }
    }
}
build_statements! {
    Documentation(()) => "documentation",
    Effect(&'a ruff_python_ast::StmtExpr) => "effect_statement",
    Binding(&'a ruff_python_ast::StmtAssign) => "binding",
    Callback(&'a ruff_python_ast::StmtFunctionDef) => "callback",
    Return(&'a ruff_python_ast::StmtReturn) => "return",
}
impl<'a> BuildStatement<'a> {
    pub(super) fn classify(stmt: &'a Stmt) -> Result<Self, String> {
        match stmt {
            Stmt::Expr(s) if matches!(&*s.value, PyExpr::StringLiteral(_)) => {
                Ok(Self::Documentation(()))
            }
            Stmt::Expr(s) if matches!(&*s.value, PyExpr::Call(_)) => Ok(Self::Effect(s)),
            Stmt::Assign(s) if s.targets.len() == 1 => Ok(Self::Binding(s)),
            Stmt::Return(s) => Ok(Self::Return(s)),
            Stmt::FunctionDef(s) => Ok(Self::Callback(s)),
            _ => Err(at(stmt, "unsupported build statement")),
        }
    }
}
impl BuildStatementKind {
    /// Syntactic inventory using production classification, not semantic admission.
    /// Callers must still compile the complete program to validate its premises.
    pub fn inventory(source: &str) -> Result<Vec<Self>, String> {
        let ast = ruff_python_parser::parse_module(source).map_err(|e| e.to_string())?;
        let [Stmt::ClassDef(class)] = ast.suite().as_slice() else {
            return Err("expected one program class".into());
        };
        let build = class
            .body
            .iter()
            .find_map(|stmt| match stmt {
                Stmt::FunctionDef(def) if def.name.as_str() == "build" => Some(def),
                _ => None,
            })
            .ok_or("missing build")?;
        build
            .body
            .iter()
            .map(|stmt| BuildStatement::classify(stmt).map(|s| s.kind()))
            .collect()
    }
}
