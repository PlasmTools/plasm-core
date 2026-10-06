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

    fn error(self, statement: &Stmt) -> PythonSourceError {
        match self {
            Self::Import => PythonSourceError::BuildImport,
            Self::While => PythonSourceError::BuildWhile,
            Self::Branch => PythonSourceError::BuildBranch,
            Self::Other => match statement {
                Stmt::Assign(assign) => PythonSourceError::BuildAssignmentTargets {
                    actual: assign.targets.len(),
                },
                Stmt::Delete(_) => PythonSourceError::BuildDelete,
                Stmt::AugAssign(_) => PythonSourceError::BuildAugmentedAssignment,
                Stmt::AnnAssign(_) => PythonSourceError::BuildAnnotatedAssignment,
                Stmt::With(_) => PythonSourceError::BuildWith,
                Stmt::Raise(_) => PythonSourceError::BuildRaise,
                Stmt::Try(_) => PythonSourceError::BuildTry,
                Stmt::Assert(_) => PythonSourceError::BuildAssert,
                Stmt::Global(_) => PythonSourceError::BuildGlobal,
                Stmt::Nonlocal(_) => PythonSourceError::BuildNonlocal,
                Stmt::Pass(_) => PythonSourceError::BuildPass,
                Stmt::Break(_) => PythonSourceError::BuildBreak,
                Stmt::Continue(_) => PythonSourceError::BuildContinue,
                Stmt::TypeAlias(_) => PythonSourceError::BuildTypeAlias,
                Stmt::ClassDef(_) => PythonSourceError::BuildNestedClass,
                Stmt::Expr(_) => PythonSourceError::BuildExpression,
                Stmt::IpyEscapeCommand(_) => PythonSourceError::BuildIpythonCommand,
                Stmt::Import(_)
                | Stmt::ImportFrom(_)
                | Stmt::While(_)
                | Stmt::If(_)
                | Stmt::Match(_)
                | Stmt::FunctionDef(_)
                | Stmt::For(_)
                | Stmt::Return(_) => {
                    unreachable!("supported or separately classified build statement")
                }
            },
        }
    }
}

pub(super) struct BuildStatementError<'a> {
    statement: &'a Stmt,
    kind: UnsupportedBuildStatement,
}

impl BuildStatementError<'_> {
    pub(super) fn correction(&self) -> PythonLoweringError {
        at(self.statement, self.kind.error(self.statement))
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
        let ast =
            ruff_python_parser::parse_module(source).map_err(PythonLoweringError::parse_error)?;
        let [Stmt::ClassDef(class)] = ast.suite().as_slice() else {
            return Err(crate::program_rejection::PythonProgramError::ProgramClassCount.into());
        };
        let build = class
            .body
            .iter()
            .find_map(|stmt| match stmt {
                Stmt::FunctionDef(def) if def.name.as_str() == "build" => Some(def),
                _ => None,
            })
            .ok_or(crate::program_rejection::PythonProgramError::BuildMethodMissing)?;
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
            let correction = error.to_string();
            assert!(correction.contains(expected), "{statement}: {error}");
            assert!(
                !correction.contains("unsupported build statement"),
                "{error}"
            );
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
