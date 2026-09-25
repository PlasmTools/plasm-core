use super::comp::PlasmComp;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewritePolicy {
    Strict,
    Optimizer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompEquivDiff {
    EffectOrder,
    ApprovalBarrier,
    BindGraph,
    ReturnParallelSet,
    StepPayload,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompEquivResult {
    Equal,
    NotEqual { reason: CompEquivDiff },
}

pub fn comp_semantic_eq(a: &PlasmComp, b: &PlasmComp) -> bool {
    crate::plasm_comp_commit_canonical(a) == crate::plasm_comp_commit_canonical(b)
}

pub fn comp_equivalent(a: &PlasmComp, b: &PlasmComp, policy: RewritePolicy) -> CompEquivResult {
    if comp_semantic_eq(a, b) {
        return CompEquivResult::Equal;
    }
    match policy {
        RewritePolicy::Strict => CompEquivResult::NotEqual {
            reason: CompEquivDiff::StepPayload,
        },
        RewritePolicy::Optimizer => {
            if a.bind.topo != b.bind.topo {
                return CompEquivResult::NotEqual {
                    reason: CompEquivDiff::BindGraph,
                };
            }
            if a.return_ != b.return_ {
                return CompEquivResult::NotEqual {
                    reason: CompEquivDiff::ReturnParallelSet,
                };
            }
            CompEquivResult::NotEqual {
                reason: CompEquivDiff::StepPayload,
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewriteClass {
    PureComputeReorder,
    PureComputeFusion,
    ReadSiblingReorder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForbiddenRewrite {
    WriteOrder,
    SideEffectBarrier,
    ApprovalGate,
    ForEachScope,
    RelationFanout,
    UsesResultOrder,
}
