//! MCP `resources/read` trace emission (keeps [`super::mod`] handler focused on resolution).

use std::time::Instant;

use plasm_trace::RunArtifactArchiveRef;

use crate::run_artifacts::ArtifactPayload;
use crate::server_state::PlasmHostState;

use super::discover::mcp_artifact_payload_chars;
use super::discover::mcp_truncate_resource_uri_display;

pub(crate) struct McpResourceReadTrace<'a> {
    pub logical_session_trace_key: Option<&'a str>,
    pub read_source: Option<&'a str>,
    pub started: Instant,
    pub uri: &'a str,
    pub archive: Option<RunArtifactArchiveRef>,
    pub payload: Option<&'a ArtifactPayload>,
    pub result: &'a str,
    pub error_class: Option<&'a str>,
}

impl<'a> McpResourceReadTrace<'a> {
    pub(crate) fn error(
        logical_session_trace_key: Option<&'a str>,
        read_source: Option<&'a str>,
        started: Instant,
        uri: &'a str,
        archive: Option<RunArtifactArchiveRef>,
        error_class: &'static str,
    ) -> Self {
        Self {
            logical_session_trace_key,
            read_source,
            started,
            uri,
            archive,
            payload: None,
            result: "error",
            error_class: Some(error_class),
        }
    }

    pub(crate) fn success(
        logical_session_trace_key: Option<&'a str>,
        read_source: Option<&'a str>,
        started: Instant,
        uri: &'a str,
        archive: Option<RunArtifactArchiveRef>,
        payload: &'a ArtifactPayload,
    ) -> Self {
        Self {
            logical_session_trace_key,
            read_source,
            started,
            uri,
            archive,
            payload: Some(payload),
            result: "success",
            error_class: None,
        }
    }

    pub(crate) async fn emit(self, plasm: &PlasmHostState) {
        let Some(mcp_key) = self.logical_session_trace_key.filter(|s| !s.is_empty()) else {
            return;
        };
        let (chars_added, is_binary) = self
            .payload
            .map(mcp_artifact_payload_chars)
            .unwrap_or((0, false));
        let duration_ms = self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        plasm
            .trace_hub
            .trace_record_mcp_resource_read(
                mcp_key,
                self.archive,
                mcp_truncate_resource_uri_display(self.uri),
                chars_added,
                is_binary,
                duration_ms,
                self.result,
                self.error_class,
                self.read_source,
            )
            .await;
    }
}
