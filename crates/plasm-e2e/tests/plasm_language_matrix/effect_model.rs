//! Independent finite effect semantics. No compiler/runtime imports.
//! A nested map admits each child in order. A system failure stops further
//! admission at every enclosing scope; earlier writes cannot be rolled back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    Rejected,
    ResponseLost,
    InvalidResponse,
}

#[derive(Debug)]
pub struct Expected {
    pub attempts: usize,
    pub committed: usize,
    pub acknowledged: usize,
    pub unresolved: usize,
    pub succeeds: bool,
}

pub fn nested_writes(parents: usize, fault: Option<(usize, Fault)>) -> Expected {
    let fails = fault.is_some_and(|(at, _)| at < parents * parents);
    let attempts = if fails {
        fault.unwrap().0 + 1
    } else {
        parents * parents
    };
    let fault = fault.filter(|_| fails).map(|(_, fault)| fault);
    Expected {
        attempts,
        committed: attempts - usize::from(fault == Some(Fault::Rejected)),
        acknowledged: attempts - usize::from(fails),
        unresolved: usize::from(matches!(fault, Some(Fault::Rejected | Fault::ResponseLost))),
        succeeds: !fails,
    }
}

#[test]
fn response_loss_is_not_evidence_of_rollback() {
    for count in [1, 3] {
        let lost = nested_writes(count, Some((0, Fault::ResponseLost)));
        assert_eq!(lost.committed, 1);
        assert_eq!(lost.acknowledged, 0);
        assert_eq!(lost.unresolved, 1);
        assert!(!lost.succeeds);
        let rejected = nested_writes(count, Some((0, Fault::Rejected)));
        assert_eq!(rejected.committed, 0);
        assert_eq!(rejected.attempts, lost.attempts);
    }
}
