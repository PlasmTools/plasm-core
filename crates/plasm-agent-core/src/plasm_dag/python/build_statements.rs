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
    For(&'a ruff_python_ast::StmtFor) => "finite_for",
    Callback(&'a ruff_python_ast::StmtFunctionDef) => "callback",
    Return(&'a ruff_python_ast::StmtReturn) => "return",
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnsupportedBuildStatement {
    Import,
    While,
    Branch,
    Other,
}

impl UnsupportedBuildStatement {
    fn recognize(stmt: &Stmt) -> Self {
        match stmt {
            Stmt::Import(_) | Stmt::ImportFrom(_) => Self::Import,
            Stmt::While(_) => Self::While,
            Stmt::If(_) | Stmt::Match(_) => Self::Branch,
            _ => Self::Other,
        }
    }

    fn correction(self) -> &'static str {
        match self {
            Self::Import => "imports are not build statements; declare permitted imports at module scope or inside @compute",
            Self::While => "build cannot expand a while loop; use bounded iterate for re-observed effects or @compute for pure Python iteration",
            Self::Branch => "build does not execute Python branches; place a conditional in a scoped callback, or filter rows before applying effects",
            Self::Other => "unsupported build statement; build admits immutable assignments, declared callbacks, standalone effect calls and an explicit return",
        }
    }
}

pub(super) struct BuildStatementError<'a> {
    statement: &'a Stmt,
    kind: UnsupportedBuildStatement,
}

impl BuildStatementError<'_> {
    pub(super) fn correction(&self) -> PythonLoweringError {
        at(self.statement, self.kind.correction())
    }
}

impl<'a> BuildStatement<'a> {
    pub(super) fn classify(stmt: &'a Stmt) -> Result<Self, BuildStatementError<'a>> {
        match stmt {
            Stmt::Expr(s) if matches!(&*s.value, PyExpr::StringLiteral(_)) => {
                Ok(Self::Documentation(()))
            }
            Stmt::Expr(s) if matches!(&*s.value, PyExpr::Call(_)) => Ok(Self::Effect(s)),
            Stmt::Assign(s) if s.targets.len() == 1 => Ok(Self::Binding(s)),
            Stmt::For(s) => Ok(Self::For(s)),
            Stmt::Return(s) => Ok(Self::Return(s)),
            Stmt::FunctionDef(s) => Ok(Self::Callback(s)),
            _ => Err(BuildStatementError {
                statement: stmt,
                kind: UnsupportedBuildStatement::recognize(stmt),
            }),
        }
    }
}
impl BuildStatementKind {
    /// Syntactic inventory using production classification, not semantic admission.
    /// Callers must still compile the complete program to validate its premises.
    pub fn inventory(source: &str) -> Result<Vec<Self>, PythonLoweringError> {
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
            .map(|stmt| {
                BuildStatement::classify(stmt)
                    .map(|s| s.kind())
                    .map_err(|e| e.correction())
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_build_constructs_carry_their_own_corrections() {
        for (statement, expected) in [
            ("import datetime", "module scope or inside @compute"),
            (
                "while True:\n            pass",
                "cannot expand a while loop",
            ),
            (
                "if flag:\n            return rows",
                "conditional in a scoped callback",
            ),
        ] {
            let source = format!("class P(Program):\n    def build(self):\n        {statement}\n");
            let error = BuildStatementKind::inventory(&source).expect_err(statement);
            assert!(error.contains(expected), "{statement}: {error}");
            assert!(!error.contains("unsupported build statement"), "{error}");
        }
    }

    #[test]
    fn finite_for_has_an_explicit_build_statement_kind() {
        let source = "class P(Program):\n    def build(self):\n        for key in ['i1']:\n            E.get(key).PING()\n        return E.get('i1')\n";
        assert_eq!(
            BuildStatementKind::inventory(source).unwrap(),
            vec![BuildStatementKind::For, BuildStatementKind::Return]
        );
    }
}
