use plasm_trace_wire::RunArtifactArchiveRef;
use serde_json::json;

#[test]
fn archive_reference_roundtrips_with_and_without_resource_index() {
    for resource_index in [None, Some(7)] {
        let archive = RunArtifactArchiveRef {
            prompt_hash: "prompt".to_owned(),
            session_id: "session".to_owned(),
            run_id: format!("pr{}", "a".repeat(64)),
            resource_index,
        };
        let encoded = serde_json::to_value(&archive).expect("serialize archive reference");
        if let Some(index) = resource_index {
            assert_eq!(encoded["resource_index"], json!(index));
        } else {
            assert!(encoded.get("resource_index").is_none());
        }
        let decoded: RunArtifactArchiveRef =
            serde_json::from_value(encoded).expect("deserialize archive reference");
        assert_eq!(decoded.prompt_hash, archive.prompt_hash);
        assert_eq!(decoded.session_id, archive.session_id);
        assert_eq!(decoded.run_id, archive.run_id);
        assert_eq!(decoded.resource_index, resource_index);
    }
}
