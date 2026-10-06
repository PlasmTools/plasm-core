//! A stable Python symbol space shared by evaluation and the local REPL.
use plasm_agent_core::{
    execute_session::ExecuteSession, program_diagnostic::ProgramDiagnostic, PlasmCompBundle,
};
use plasm_core::{CgsContext, TeachingExposureSession, CGS};
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProgramSessionError {
    #[error("unknown entity `{entity}`")]
    UnknownEntity { entity: String },
    #[error("teaching exposure is missing from the execute session")]
    MissingExposure,
    #[error("failed to prepare Python teaching wave")]
    TeachingWave(#[from] plasm_core::prompt_render::python::PythonTeachingError),
    #[error("failed to compile capability templates: {0}")]
    CapabilityTemplates(#[source] plasm_compile::CatalogTemplateError),
}

#[derive(Debug)]
pub enum ProgramCompileFailure {
    Program(Box<ProgramDiagnostic>),
    Host(plasm_agent_core::compilation_error::ExecutionFailure),
}
impl std::fmt::Display for ProgramCompileFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.agent_markdown())
    }
}
impl std::error::Error for ProgramCompileFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Program(diagnostic) => Some(&diagnostic.stage),
            Self::Host(failure) => Some(failure),
        }
    }
}
impl ProgramCompileFailure {
    pub fn agent_markdown(&self) -> String {
        match self {
            Self::Program(diagnostic) => diagnostic.agent_markdown(),
            Self::Host(failure) => failure.to_string(),
        }
    }
    pub fn into_program(
        self,
    ) -> Result<ProgramDiagnostic, plasm_agent_core::compilation_error::ExecutionFailure> {
        match self {
            Self::Program(diagnostic) => Ok(*diagnostic),
            Self::Host(failure) => Err(failure),
        }
    }
}

pub struct ProgramSession {
    pub execute: ExecuteSession,
}

impl ProgramSession {
    pub fn new(cgs: &CGS, focus: Option<&str>) -> Result<Self, ProgramSessionError> {
        let mut local_cgs = cgs.clone();
        local_cgs.bind_registry_entry_id("local");
        let cgs = Arc::new(local_cgs);
        let mut entities = match focus {
            Some(entity) => {
                if cgs.get_entity(entity).is_none() {
                    return Err(ProgramSessionError::UnknownEntity {
                        entity: entity.to_string(),
                    });
                }
                vec![entity.to_string()]
            }
            None => cgs.entities.keys().map(ToString::to_string).collect(),
        };
        entities.sort();
        let refs = entities.iter().map(String::as_str).collect::<Vec<_>>();
        let exposure = TeachingExposureSession::new(&cgs, "local", &refs);
        let wave = plasm_core::prompt_render::python::prepare_python_teaching_wave(
            &exposure,
            &Default::default(),
        )?;
        let prompt = format!(
            "{}\n\n```text\n{}\n```\n",
            wave.language.unwrap_or_default(),
            wave.declarations
        );
        let compiled = Arc::new(
            plasm_compile::compile_cgs_capability_templates(&cgs)
                .map_err(ProgramSessionError::CapabilityTemplates)?,
        );
        let contexts = [(
            "local".into(),
            Arc::new(CgsContext::entry("local", cgs.clone())),
        )]
        .into_iter()
        .collect();
        let mut execute = ExecuteSession::new_with_bindings(
            cgs.catalog_cgs_hash_hex(),
            prompt,
            cgs.clone(),
            contexts,
            "local".into(),
            String::new(),
            String::new(),
            None,
            entities,
            Some(exposure),
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            Default::default(),
            [("local".into(), compiled)].into_iter().collect(),
        );
        execute.python_teaching = wave.next_state;
        Ok(Self { execute })
    }

    pub fn extend(&mut self, entity: &str) -> Result<String, ProgramSessionError> {
        let cgs = self.execute.cgs.clone();
        if cgs.get_entity(entity).is_none() {
            return Err(ProgramSessionError::UnknownEntity {
                entity: entity.to_string(),
            });
        }
        let mut exposure = self
            .execute
            .teaching_exposure
            .clone()
            .ok_or(ProgramSessionError::MissingExposure)?;
        exposure.expose_entities(&[&cgs], cgs.clone(), "local", &[entity]);
        let wave = plasm_core::prompt_render::python::prepare_python_teaching_wave(
            &exposure,
            &self.execute.python_teaching,
        )?;
        let delta = if wave.declarations.is_empty() {
            String::new()
        } else {
            format!("\n```text\n{}\n```\n", wave.declarations)
        };
        self.execute.prompt_text.push_str(&delta);
        self.execute.entities = exposure.entities.clone();
        self.execute.teaching_exposure = Some(exposure);
        self.execute.python_teaching = wave.next_state;
        Ok(delta)
    }

    pub fn prompt(&self) -> &str {
        &self.execute.prompt_text
    }

    pub async fn compile(&self, source: &str) -> Result<PlasmCompBundle, ProgramCompileFailure> {
        let pipeline = Default::default();
        plasm_agent_core::compile_program(&pipeline, None, &self.execute, "program", source)
            .await
            .map_err(|error| match error {
                plasm_agent_core::compilation_error::CompilationError::Program(error) => {
                    ProgramCompileFailure::Program(Box::new(ProgramDiagnostic::from_stage(
                        &pipeline,
                        None,
                        &self.execute,
                        source,
                        *error,
                    )))
                }
                plasm_agent_core::compilation_error::CompilationError::Host(failure) => {
                    ProgramCompileFailure::Host(failure)
                }
                error @ plasm_agent_core::compilation_error::CompilationError::Checker(_) => {
                    ProgramCompileFailure::Host(error.into())
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_error_footprints_are_bounded() {
        let session_bytes = std::mem::size_of::<ProgramSessionError>();
        let compile_bytes = std::mem::size_of::<ProgramCompileFailure>();
        let reference_bytes = std::mem::size_of::<crate::ProgramReferenceError>();
        assert!(
            session_bytes < 128,
            "ProgramSessionError is {session_bytes} bytes; expected <128"
        );
        assert!(
            compile_bytes < 128,
            "ProgramCompileFailure is {compile_bytes} bytes; expected <128"
        );
        assert!(
            reference_bytes < 128,
            "ProgramReferenceError is {reference_bytes} bytes; expected <128"
        );
    }

    #[test]
    fn boxed_diagnostic_preserves_display_source_and_owned_metadata() {
        use plasm_agent_core::program_diagnostic::{
            ProgramScore, ProgramStageError, UnderstoodSketch,
        };
        use std::error::Error;
        let diagnostic = ProgramDiagnostic {
            stage: ProgramStageError::CoreType {
                error: plasm_core::TypeError::FieldNotFound {
                    field: "score".into(),
                    entity: "Item".into(),
                },
            },
            score: ProgramScore {
                overall: 0.4,
                parse: 1.0,
                type_check: 0.0,
                plan: 0.0,
            },
            understood: Some(UnderstoodSketch {
                bindings: vec!["items".into()],
                failed_at: Some("score".into()),
                sketch: None,
                ok_count: 1,
                total: 2,
            }),
            replay: None,
        };
        let display = diagnostic.agent_markdown();
        let failure = ProgramCompileFailure::Program(Box::new(diagnostic));
        assert_eq!(failure.to_string(), display);
        let stage = failure
            .source()
            .unwrap()
            .downcast_ref::<ProgramStageError>()
            .unwrap();
        assert!(
            matches!(stage.source().unwrap().downcast_ref::<plasm_core::TypeError>(),
                Some(plasm_core::TypeError::FieldNotFound { field, entity })
                    if field == "score" && entity == "Item"
            )
        );
        let owned = failure.into_program().unwrap();
        assert_eq!(owned.score.overall, 0.4);
        let understood = owned.understood.unwrap();
        assert_eq!(understood.bindings, ["items"]);
        assert_eq!(understood.failed_at.as_deref(), Some("score"));
        assert_eq!((understood.ok_count, understood.total), (1, 2));
    }

    fn session() -> ProgramSession {
        let cgs = plasm_core::load_schema(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/python_dag_slice"),
        )
        .unwrap();
        ProgramSession::new(&cgs, Some("Item")).unwrap()
    }
    #[test]
    fn unknown_focus_is_a_typed_semantic_error() {
        let cgs = plasm_core::load_schema(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/python_dag_slice"),
        )
        .unwrap();
        assert!(matches!(
            ProgramSession::new(&cgs, Some("MissingEntity")),
            Err(ProgramSessionError::UnknownEntity { entity }) if entity == "MissingEntity"
        ));
    }
    #[tokio::test]
    async fn same_session_teaches_compiles_and_repairs_python_only() {
        let session = session();
        assert!(session
            .prompt()
            .starts_with(plasm_core::prompt_render::python::LANGUAGE));
        assert!(session.prompt().contains("e1:"));
        assert!(session.compile("e1").await.is_err());
        let source = "class Read(Program):\n    def build(self):\n        return e1.query()\n";
        let bundle = session.compile(source).await.unwrap();
        assert!(!bundle.artifact().comp.steps.is_empty());
        let malformed = "class Read(Program):\n    def build(self):\n        return (\n";
        let first = session
            .compile(malformed)
            .await
            .unwrap_err()
            .into_program()
            .expect("program rejection")
            .agent_meta("local", 0);
        let repeat = session
            .compile(malformed)
            .await
            .unwrap_err()
            .into_program()
            .expect("program rejection")
            .agent_meta("local", 0);
        assert_ne!(
            first, repeat,
            "repeat repair includes production rejection memory"
        );
    }
}
