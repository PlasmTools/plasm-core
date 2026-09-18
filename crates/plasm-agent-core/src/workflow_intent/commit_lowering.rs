//! Lower structured ownership before publishing any workflow or question state.
use super::{
    CommitBody, CommitDraft, CommitNode, CommitStatement, DecisionDraft, Question, Questions,
    WorkflowIntent,
};
use crate::workflow_intent::{conditional, InterpretationDraft, RequirementDraft, RequirementKind};
use anyhow::{ensure, Context, Result};
use std::collections::BTreeSet;

type DecisionOwner = Option<(String, DecisionDraft)>;
struct Lowering<'a> {
    original: &'a Questions,
    questions: Questions,
    visible: Vec<String>,
    requirements: Vec<RequirementDraft>,
    references: Vec<Option<String>>,
    conditionals: Vec<conditional::Draft>,
}
impl Lowering<'_> {
    fn push(
        &mut self,
        s: CommitStatement,
        kind: RequirementKind,
        owner: &DecisionOwner,
    ) -> Result<()> {
        ensure!(self.requirements.len() < 64, "too many requirements");
        super::provenance(&s.source_turn_ids, &self.visible, false)?;
        ensure!(!s.statement.trim().is_empty(), "blank requirement");
        self.references
            .push(owner.as_ref().map(|(id, _)| id.clone()));
        self.requirements.push(RequirementDraft {
            kind,
            statement: s.statement,
            source_turn_ids: s.source_turn_ids,
            uncertainty: owner.as_ref().map(|(_, d)| d.clone()),
        });
        Ok(())
    }
    fn body(&mut self, body: CommitBody, inherited: &DecisionOwner, depth: usize) -> Result<()> {
        ensure!(
            depth <= conditional::MAX_DEPTH,
            "conditional nesting exceeds depth limit"
        );
        let expected: BTreeSet<_> = self
            .original
            .entries
            .iter()
            .filter(|q| q.resolution.is_none())
            .map(|q| q.id.clone())
            .collect();
        ensure!(
            body.existing_questions
                .keys()
                .cloned()
                .collect::<BTreeSet<_>>()
                == expected,
            "existing question group totality"
        );
        self.nodes(body.settled_requirements, inherited, depth)?;
        for (id, nodes) in body.existing_questions {
            let q = self
                .original
                .entries
                .iter()
                .find(|q| q.id == id && q.resolution.is_none())
                .context("unknown or resolved question ID")?;
            let owner = Some((
                id,
                DecisionDraft {
                    question: q.question.clone(),
                    alternatives: q.alternatives.clone(),
                },
            ));
            self.nodes(nodes, &owner, depth)?;
        }
        for group in body.new_questions {
            ensure!(
                !group.requirements.is_empty(),
                "new question requires an owner"
            );
            ensure!(
                self.questions.entries.len() - self.original.entries.len() < 16,
                "too many new questions"
            );
            let decision = DecisionDraft {
                question: group.question,
                alternatives: group.alternatives,
            };
            decision.validate()?;
            let id = format!("q{}", self.questions.entries.len());
            // Reserve identity before lowering nested declarations; provenance
            // is the union of the nodes actually owned by this question.
            let position = self.questions.entries.len();
            self.questions.entries.push(Question {
                id: id.clone(),
                question: decision.question.clone(),
                alternatives: decision.alternatives.clone(),
                source_turn_ids: vec![],
                resolution: None,
            });
            let start = self.requirements.len();
            self.nodes(group.requirements, &Some((id.clone(), decision)), depth)?;
            let sources: BTreeSet<_> = self.requirements[start..]
                .iter()
                .zip(&self.references[start..])
                .filter(|(_, reference)| reference.as_ref() == Some(&id))
                .flat_map(|(r, _)| r.source_turn_ids.iter().cloned())
                .collect();
            ensure!(!sources.is_empty(), "new question requires an actual owner");
            self.questions.entries[position].source_turn_ids = sources.into_iter().collect();
        }
        Ok(())
    }
    fn nodes(&mut self, nodes: Vec<CommitNode>, owner: &DecisionOwner, depth: usize) -> Result<()> {
        for node in nodes {
            match node {
                CommitNode::Effect(s) => self.push(s, RequirementKind::Effect, owner)?,
                CommitNode::InformationNeed(s) => {
                    self.push(s, RequirementKind::InformationNeed, owner)?
                }
                CommitNode::SelectionConstraint(s) => {
                    self.push(s, RequirementKind::SelectionConstraint, owner)?
                }
                CommitNode::Prohibition(s) => {
                    ensure!(depth == 0, "prohibition cannot be a branch member");
                    self.push(s, RequirementKind::Prohibition, owner)?;
                }
                CommitNode::Conditional(c) => {
                    ensure!(
                        depth < conditional::MAX_DEPTH,
                        "conditional nesting exceeds depth limit"
                    );
                    let predicate = self.requirements.len();
                    self.push(c.predicate, RequirementKind::Condition, owner)?;
                    let start = self.requirements.len();
                    self.body(c.when_true, owner, depth + 1)?;
                    let middle = self.requirements.len();
                    self.body(c.when_false, owner, depth + 1)?;
                    self.conditionals.push(conditional::Draft {
                        predicate,
                        when_true: (start..middle).collect(),
                        when_false: (middle..self.requirements.len()).collect(),
                    });
                }
            }
        }
        Ok(())
    }
}

pub(super) fn lower(
    w: &WorkflowIntent,
    q: &Questions,
    proposed: CommitDraft,
) -> Result<(InterpretationDraft, Questions, Vec<Option<String>>)> {
    let mut lower = Lowering {
        original: q,
        questions: q.clone(),
        visible: super::visible_ids(w),
        requirements: vec![],
        references: vec![],
        conditionals: vec![],
    };
    lower.body(
        CommitBody {
            settled_requirements: proposed.settled_requirements,
            existing_questions: proposed.existing_questions,
            new_questions: proposed.new_questions,
        },
        &None,
        0,
    )?;
    let draft = InterpretationDraft {
        dispositions: conditional::expand_dispositions(w.interpretation(), proposed.dispositions)?,
        requirements: lower.requirements,
        conditionals: lower.conditionals,
        no_requirements_reason: proposed.no_requirements_reason,
    };
    Ok((draft, lower.questions, lower.references))
}
