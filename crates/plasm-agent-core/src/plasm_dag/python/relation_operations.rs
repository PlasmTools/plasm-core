//! Relation navigation is shared by root expressions and correlated row bodies.
use super::*;

macro_rules! relation_operations {
    ($($variant:ident => $name:literal),+ $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum RelationOperation { $($variant),+ }
        impl RelationOperation {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            pub fn name(self) -> &'static str { match self { $(Self::$variant => $name),+ } }
        }
    }
}
relation_operations! { Navigate => "navigate" }

impl RelationOperation {
    pub(super) fn lower(
        self,
        lower: &mut Lower<'_>,
        site: &PyExpr,
        source: &str,
        relation: &str,
        id: &str,
    ) -> Result<String, String> {
        match self {
            Self::Navigate => {
                let contract = super::super::binding_contract(&lower.state, source)
                    .ok_or("missing relation source contract")?;
                if !contract.row_cardinality.permits_scalar_field_extract() {
                    return Err(at(
                        site,
                        "relation dot requires a singleton; use flat_map for plural rows",
                    ));
                }
                // The shared relation lowerer retains ownership, anchor, scope and
                // materialization proofs. Correlated bodies supply their local row.
                let node = super::super::binding_continuation::lower_relation_application(
                    lower.es,
                    &lower.state,
                    id,
                    "",
                    source,
                    relation,
                )?;
                lower.insert(node)
            }
        }
    }
}
