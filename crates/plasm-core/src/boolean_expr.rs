//! Boolean structure shared by surface predicates and serialized row filters.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "args", rename_all = "snake_case")]
pub enum BooleanExpr<T> {
    Atom(T),
    And(Vec<Self>),
    Or(Vec<Self>),
    Not(Box<Self>),
}

impl<T> BooleanExpr<T> {
    pub fn has_empty_branch(&self) -> bool {
        match self {
            Self::Atom(_) => false,
            Self::And(args) | Self::Or(args) => {
                args.is_empty() || args.iter().any(Self::has_empty_branch)
            }
            Self::Not(arg) => arg.has_empty_branch(),
        }
    }

    pub fn iter(&self) -> Box<dyn Iterator<Item = &T> + '_> {
        match self {
            Self::Atom(value) => Box::new(std::iter::once(value)),
            Self::And(args) | Self::Or(args) => Box::new(args.iter().flat_map(Self::iter)),
            Self::Not(arg) => arg.iter(),
        }
    }

    pub fn try_map<U, E>(
        &self,
        f: &mut impl FnMut(&T) -> Result<U, E>,
    ) -> Result<BooleanExpr<U>, E> {
        Ok(match self {
            Self::Atom(value) => BooleanExpr::Atom(f(value)?),
            Self::And(args) => BooleanExpr::And(
                args.iter()
                    .map(|a| a.try_map(f))
                    .collect::<Result<_, _>>()?,
            ),
            Self::Or(args) => BooleanExpr::Or(
                args.iter()
                    .map(|a| a.try_map(f))
                    .collect::<Result<_, _>>()?,
            ),
            Self::Not(arg) => BooleanExpr::Not(Box::new(arg.try_map(f)?)),
        })
    }

    pub fn try_flat_map<U, E>(
        &self,
        f: &mut impl FnMut(&T) -> Result<BooleanExpr<U>, E>,
    ) -> Result<BooleanExpr<U>, E> {
        Ok(match self {
            Self::Atom(value) => f(value)?,
            Self::And(args) => BooleanExpr::And(
                args.iter()
                    .map(|a| a.try_flat_map(f))
                    .collect::<Result<_, _>>()?,
            ),
            Self::Or(args) => BooleanExpr::Or(
                args.iter()
                    .map(|a| a.try_flat_map(f))
                    .collect::<Result<_, _>>()?,
            ),
            Self::Not(arg) => BooleanExpr::Not(Box::new(arg.try_flat_map(f)?)),
        })
    }

    /// Kleene logic matches nullable row comparisons; only Some(true) selects a row.
    pub fn evaluate(&self, atom: &impl Fn(&T) -> Option<bool>) -> Option<bool> {
        match self {
            Self::Atom(value) => atom(value),
            Self::Not(arg) => arg.evaluate(atom).map(|v| !v),
            Self::And(args) => {
                args.iter()
                    .fold(Some(true), |acc, arg| match (acc, arg.evaluate(atom)) {
                        (Some(false), _) | (_, Some(false)) => Some(false),
                        (Some(true), Some(true)) => Some(true),
                        _ => None,
                    })
            }
            Self::Or(args) => {
                args.iter()
                    .fold(Some(false), |acc, arg| match (acc, arg.evaluate(atom)) {
                        (Some(true), _) | (_, Some(true)) => Some(true),
                        (Some(false), Some(false)) => Some(false),
                        _ => None,
                    })
            }
        }
    }

    pub fn render(&self, atom: &impl Fn(&T) -> String) -> String {
        match self {
            Self::Atom(value) => atom(value),
            Self::And(args) => format!(
                "({})",
                args.iter()
                    .map(|a| a.render(atom))
                    .collect::<Vec<_>>()
                    .join(" AND ")
            ),
            Self::Or(args) => format!(
                "({})",
                args.iter()
                    .map(|a| a.render(atom))
                    .collect::<Vec<_>>()
                    .join(" OR ")
            ),
            Self::Not(arg) => format!("NOT ({})", arg.render(atom)),
        }
    }

    /// Only conjunctions can enter optimizations whose contract is a flat AND.
    pub fn conjunction(&self) -> Option<Vec<T>>
    where
        T: Clone,
    {
        match self {
            Self::Atom(value) => Some(vec![value.clone()]),
            Self::And(args) => args.iter().try_fold(Vec::new(), |mut out, arg| {
                out.extend(arg.conjunction()?);
                Some(out)
            }),
            Self::Or(_) | Self::Not(_) => None,
        }
    }
}

impl<T> From<Vec<T>> for BooleanExpr<T> {
    fn from(values: Vec<T>) -> Self {
        Self::And(values.into_iter().map(Self::Atom).collect())
    }
}
impl<'a, T> IntoIterator for &'a BooleanExpr<T> {
    type Item = &'a T;
    type IntoIter = Box<dyn Iterator<Item = &'a T> + 'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
