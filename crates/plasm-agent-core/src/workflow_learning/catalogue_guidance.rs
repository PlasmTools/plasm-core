//! Reviewed catalogue associations are independent of example disclosure eligibility.
use super::{
    index::NormalizedIntent,
    model::{CompiledProgramId, RunArtifactId, SealedReview},
    normalization::FamilyMatch,
    repository::RepositoryError,
    teaching::ProgramCatalogueRequirements,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "evidence", rename_all = "snake_case")]
pub enum CatalogueRequirementEvidence {
    Available(ProgramCatalogueRequirements),
    Unavailable { diagnostic: String },
}
impl Default for CatalogueRequirementEvidence {
    fn default() -> Self {
        Self::Unavailable {
            diagnostic: "historical receipt has no compiled catalogue witness".into(),
        }
    }
}
impl CatalogueRequirementEvidence {
    pub fn from_compiled(
        session: &crate::execute_session::ExecuteSession,
        bundle: &crate::PlasmCompBundle,
    ) -> Self {
        match ProgramCatalogueRequirements::from_compiled(session, bundle) {
            Ok(requirements) => Self::Available(requirements),
            Err(error) => Self::Unavailable {
                diagnostic: error.to_string(),
            },
        }
    }

    pub fn available(&self) -> Option<&ProgramCatalogueRequirements> {
        match self {
            Self::Available(requirements) => Some(requirements),
            Self::Unavailable { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogueAssociation {
    pub(super) family: String,
    pub(super) variant: String,
    pub(super) intent: NormalizedIntent,
    pub(super) embedding: Vec<f32>,
    pub(super) embedding_profile: String,
    pub(super) requirements: ProgramCatalogueRequirements,
    pub(super) evidence: BTreeMap<RunArtifactId, String>,
    pub(super) confirmed: bool,
}
impl CatalogueAssociation {
    pub fn from_review(
        seal: &SealedReview,
        run_ids: &[RunArtifactId],
        intent: NormalizedIntent,
        family: FamilyMatch,
        embedding: Vec<f32>,
        embedding_profile: String,
    ) -> Result<Self, RepositoryError> {
        let selected: Vec<_> = seal
            .executions
            .iter()
            .filter(|receipt| run_ids.contains(&receipt.run_id))
            .collect();
        let observations =
            super::quality::program_observations(selected.iter().copied()).map_err(|_| {
                RepositoryError::InvalidEvidence(
                    "catalogue association requires one consistent program observation",
                )
            })?;
        if observations.len() != 1 || selected.len() != run_ids.len() {
            return Err(RepositoryError::InvalidEvidence(
                "catalogue association requires exact cited program receipts",
            ));
        }
        let requirements = selected[0]
            .catalogue_requirements
            .available()
            .ok_or(RepositoryError::InvalidEvidence(
                "compiled catalogue requirements unavailable",
            ))?
            .clone();
        let evidence = selected
            .into_iter()
            .map(|receipt| {
                Ok((
                    receipt.run_id,
                    plasm_core::catalog_discovery::content_hash(&serde_json::to_vec(receipt)?),
                ))
            })
            .collect::<Result<BTreeMap<_, _>, RepositoryError>>()?;
        let variant = plasm_core::catalog_discovery::content_hash(&serde_json::to_vec(&(
            requirements.catalog_fingerprints(),
            requirements.required_capabilities(),
            requirements.dependencies(),
            &embedding_profile,
        ))?);
        let confirmed = !matches!(family, FamilyMatch::Uncertain);
        let family = match family {
            FamilyMatch::Existing { family } => family.as_str().to_owned(),
            _ => intent.family_key(),
        };
        let association = Self {
            family,
            variant,
            intent,
            embedding,
            embedding_profile,
            requirements,
            evidence,
            confirmed,
        };
        association.validate(seal)?;
        Ok(association)
    }
    pub(super) fn validate(&self, seal: &SealedReview) -> Result<(), RepositoryError> {
        if self.evidence.is_empty()
            || self.evidence.len() > 1024
            || self.intent.purpose.chars().count() > 2048
            || self.intent.facets.len() > 32
            || self.intent.aliases.len() > 8
            || self
                .intent
                .facets
                .iter()
                .any(|text| text.trim().is_empty() || text.chars().count() > 1024)
            || self
                .intent
                .aliases
                .iter()
                .any(|text| text.trim().is_empty() || text.chars().count() > 512)
            || serde_json::to_vec(self)?.len() > 1_048_576
            || self.embedding_profile.trim().is_empty()
            || self.embedding.len() > 8192
            || self.embedding.iter().any(|v| !v.is_finite())
            || self.intent.purpose.trim().is_empty()
            || self.family.trim().is_empty()
            || self.requirements.required_capabilities().is_empty()
        {
            return Err(RepositoryError::InvalidEvidence(
                "invalid catalogue association contract",
            ));
        }
        let expected_variant =
            plasm_core::catalog_discovery::content_hash(&serde_json::to_vec(&(
                self.requirements.catalog_fingerprints(),
                self.requirements.required_capabilities(),
                self.requirements.dependencies(),
                &self.embedding_profile,
            ))?);
        if self.variant != expected_variant {
            return Err(RepositoryError::InvalidEvidence(
                "association contract variant differs",
            ));
        }
        for (run, digest) in &self.evidence {
            let receipt = seal.executions.iter().find(|r| &r.run_id == run).ok_or(
                RepositoryError::InvalidEvidence("association receipt absent from seal"),
            )?;
            if receipt.scope != seal.request.scope
                || receipt.logical_session != seal.request.logical_session
                || receipt.catalogue_requirements.available() != Some(&self.requirements)
                || plasm_core::catalog_discovery::content_hash(&serde_json::to_vec(receipt)?)
                    != *digest
                || plasm_core::catalog_discovery::content_hash(receipt.source.as_bytes())
                    != self.requirements.source_sha256()
                || plasm_core::catalog_discovery::content_hash(&serde_json::to_vec(
                    &receipt.compiled_plan,
                )?) != self.requirements.compiled_sha256()
                || CompiledProgramId::from_comp(&serde_json::from_value::<plasm_core::PlasmComp>(
                    receipt.compiled_plan.clone(),
                )?)? != receipt.plan_id
                || self
                    .requirements
                    .catalog_fingerprints()
                    .iter()
                    .any(|(entry, hash)| receipt.catalog_fingerprints.get(entry) != Some(hash))
            {
                return Err(RepositoryError::InvalidEvidence(
                    "association does not match sealed compiled requirements",
                ));
            }
        }
        if self.requirements.required_capabilities().iter().any(|cap| {
            !self
                .requirements
                .catalog_fingerprints()
                .contains_key(&cap.catalog)
        }) || self.requirements.dependencies().iter().any(|seed| {
            !self
                .requirements
                .catalog_fingerprints()
                .contains_key(&seed.entry_id)
        }) {
            return Err(RepositoryError::InvalidEvidence(
                "association catalogue pin missing",
            ));
        }
        Ok(())
    }
    pub fn requirements(&self) -> &ProgramCatalogueRequirements {
        &self.requirements
    }
    pub fn family(&self) -> &str {
        &self.family
    }
    pub fn intent(&self) -> &NormalizedIntent {
        &self.intent
    }
}
