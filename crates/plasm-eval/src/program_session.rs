//! A stable Python symbol space shared by evaluation and the local REPL.
use plasm_agent_core::{
    execute_session::ExecuteSession, program_diagnostic::ProgramDiagnostic, PlasmCompBundle,
};
use plasm_core::{CgsContext, TeachingExposureSession, CGS};
use std::sync::Arc;

pub struct ProgramSession {
    pub execute: ExecuteSession,
}

impl ProgramSession {
    pub fn new(cgs: &CGS, focus: Option<&str>) -> Result<Self, String> {
        let mut local_cgs = cgs.clone();
        local_cgs.bind_registry_entry_id("local");
        let cgs = Arc::new(local_cgs);
        let mut entities = match focus {
            Some(entity) => {
                if cgs.get_entity(entity).is_none() {
                    return Err(format!("unknown entity {entity}"));
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
            "{}\n\n```pyi\n{}\n```\n",
            wave.language.unwrap_or_default(),
            wave.declarations
        );
        let compiled = Arc::new(
            plasm_compile::compile_cgs_capability_templates(&cgs).map_err(|e| e.to_string())?,
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

    pub fn extend(&mut self, entity: &str) -> Result<String, String> {
        let cgs = self.execute.cgs.clone();
        if cgs.get_entity(entity).is_none() {
            return Err(format!("unknown entity {entity}"));
        }
        let mut exposure = self
            .execute
            .teaching_exposure
            .clone()
            .ok_or("missing exposure")?;
        exposure.expose_entities(&[&cgs], cgs.clone(), "local", &[entity]);
        let wave = plasm_core::prompt_render::python::prepare_python_teaching_wave(
            &exposure,
            &self.execute.python_teaching,
        )?;
        let delta = if wave.declarations.is_empty() {
            String::new()
        } else {
            format!("\n```pyi\n{}\n```\n", wave.declarations)
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

    // Preserve the shared structured diagnostic at this cold public boundary.
    #[allow(clippy::result_large_err)]
    pub fn compile(&self, source: &str) -> Result<PlasmCompBundle, ProgramDiagnostic> {
        let pipeline = Default::default();
        plasm_agent_core::compile_program(&pipeline, None, &self.execute, "program", source)
            .map_err(|error| {
                ProgramDiagnostic::from_stage(&pipeline, None, &self.execute, source, error)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn session() -> ProgramSession {
        let cgs = plasm_core::load_schema(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/python_dag_slice"),
        )
        .unwrap();
        ProgramSession::new(&cgs, Some("Item")).unwrap()
    }
    #[test]
    fn same_session_teaches_compiles_and_repairs_python_only() {
        let session = session();
        assert!(session.prompt().contains("class Program:"));
        assert!(session.prompt().contains("class e1"));
        assert!(session.compile("e1").is_err());
        let source = "class Read(Program):\n    def build(self):\n        return e1.query()\n";
        let bundle = session.compile(source).unwrap();
        assert!(!bundle.artifact().comp.steps.is_empty());
        let malformed = "class Read(Program):\n    def build(self):\n        return (\n";
        let first = session
            .compile(malformed)
            .unwrap_err()
            .agent_meta("local", 0);
        let repeat = session
            .compile(malformed)
            .unwrap_err()
            .agent_meta("local", 0);
        assert_ne!(
            first, repeat,
            "repeat repair includes production rejection memory"
        );
    }
}
