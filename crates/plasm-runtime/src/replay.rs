use crate::RuntimeError;
use blake3::Hasher;
use plasm_compile::{CompiledOperation, CompiledRequest};
use plasm_core::Value;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// A recorded request/response pair with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayEntry {
    pub request: CompiledOperation,
    pub response: serde_json::Value,
    pub decoded_entities: Vec<serde_json::Value>,
    pub schema_snapshot: serde_json::Value, // CGS as JSON
    pub timestamp: u64,
}

/// Fingerprint for deterministic request identification
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RequestFingerprint([u8; 32]);

/// Storage for recorded request/response pairs
pub trait ReplayStore: Send + Sync {
    /// Store a replay entry
    fn store(
        &mut self,
        fingerprint: RequestFingerprint,
        entry: ReplayEntry,
    ) -> Result<(), RuntimeError>;

    /// Lookup a replay entry by fingerprint
    fn lookup(&self, fingerprint: &RequestFingerprint)
        -> Result<Option<ReplayEntry>, RuntimeError>;

    /// List all stored fingerprints
    fn list_fingerprints(&self) -> Result<Vec<RequestFingerprint>, RuntimeError>;

    /// Clear all stored entries
    fn clear(&mut self) -> Result<(), RuntimeError>;
}

/// File-system based replay store
#[derive(Debug)]
pub struct FileReplayStore {
    base_path: PathBuf,
}

/// In-memory replay store for testing
#[derive(Debug, Default)]
pub struct MemoryReplayStore {
    entries: HashMap<RequestFingerprint, ReplayEntry>,
}

impl RequestFingerprint {
    /// Create a fingerprint from a compiled HTTP request.
    pub fn from_request(request: &CompiledRequest) -> Self {
        Self::from_operation(&CompiledOperation::Http(request.clone()))
    }

    /// Create a fingerprint from a compiled operation.
    pub fn from_operation(request: &CompiledOperation) -> Self {
        let mut hasher = Hasher::new();

        match request {
            CompiledOperation::Http(request) => {
                hasher.update(b"http");
                hasher.update(request.method_str().as_bytes());
                hasher.update(request.path.as_bytes());
                match request.body_format {
                    plasm_compile::HttpBodyFormat::Json => {
                        hasher.update(b"json");
                    }
                    plasm_compile::HttpBodyFormat::FormUrlencoded => {
                        hasher.update(b"form");
                    }
                    plasm_compile::HttpBodyFormat::Multipart => {
                        hasher.update(b"multipart");
                    }
                };

                if let Some(body) = &request.body {
                    let normalized_body = normalize_json_for_fingerprint(body);
                    hasher.update(normalized_body.as_bytes());
                }

                if let Some(mp) = &request.multipart {
                    hasher.update(b"|mp|");
                    for part in &mp.parts {
                        hasher.update(b"|p|");
                        hasher.update(part.name.as_bytes());
                        if let Some(fname) = &part.file_name {
                            hasher.update(b"|fn|");
                            hasher.update(fname.as_bytes());
                        }
                        if let Some(ct) = &part.content_type {
                            hasher.update(b"|ct|");
                            hasher.update(ct.as_bytes());
                        }
                        let nb = normalize_json_for_fingerprint(&part.content);
                        hasher.update(nb.as_bytes());
                    }
                }

                if let Some(query) = &request.query {
                    let normalized_query = normalize_json_for_fingerprint(query);
                    hasher.update(normalized_query.as_bytes());
                }
            }
            CompiledOperation::GraphQl(request) => {
                hasher.update(b"graphql");
                hasher.update(request.method_str().as_bytes());
                hasher.update(request.path.as_bytes());
                match request.body_format {
                    plasm_compile::HttpBodyFormat::Json => {
                        hasher.update(b"json");
                    }
                    plasm_compile::HttpBodyFormat::FormUrlencoded => {
                        hasher.update(b"form");
                    }
                    plasm_compile::HttpBodyFormat::Multipart => {
                        hasher.update(b"multipart");
                    }
                };

                if let Some(body) = &request.body {
                    let normalized_body = normalize_json_for_fingerprint(body);
                    hasher.update(normalized_body.as_bytes());
                }

                if let Some(mp) = &request.multipart {
                    hasher.update(b"|mp|");
                    for part in &mp.parts {
                        hasher.update(b"|p|");
                        hasher.update(part.name.as_bytes());
                        if let Some(fname) = &part.file_name {
                            hasher.update(b"|fn|");
                            hasher.update(fname.as_bytes());
                        }
                        if let Some(ct) = &part.content_type {
                            hasher.update(b"|ct|");
                            hasher.update(ct.as_bytes());
                        }
                        let nb = normalize_json_for_fingerprint(&part.content);
                        hasher.update(nb.as_bytes());
                    }
                }

                if let Some(query) = &request.query {
                    let normalized_query = normalize_json_for_fingerprint(query);
                    hasher.update(normalized_query.as_bytes());
                }
            }
            CompiledOperation::EvmCall(request) => {
                hasher.update(b"evm_call");
                hasher.update(&request.chain.id().to_le_bytes());
                hasher.update(request.contract.as_slice());
                hasher.update(request.function.signature().as_bytes());
                hasher.update(normalize_serde_for_fingerprint(&request.args).as_bytes());
                if let Some(block) = &request.block {
                    hasher.update(normalize_serde_for_fingerprint(block).as_bytes());
                }
                hasher.update(normalize_serde_for_fingerprint(&request.decode).as_bytes());
            }
            CompiledOperation::EvmLogs(request) => {
                hasher.update(b"evm_logs");
                hasher.update(&request.chain.id().to_le_bytes());
                hasher.update(request.contract.as_slice());
                hasher.update(request.event.signature().as_bytes());
                hasher.update(normalize_serde_for_fingerprint(&request.indexed_filters).as_bytes());
                hasher.update(normalize_serde_for_fingerprint(&request.from_block).as_bytes());
                hasher.update(normalize_serde_for_fingerprint(&request.to_block).as_bytes());
                hasher.update(normalize_serde_for_fingerprint(&request.decode).as_bytes());
            }
            CompiledOperation::View(view) => {
                hasher.update(b"view");
                hasher.update(view.view.as_bytes());
            }
        }

        RequestFingerprint(*hasher.finalize().as_bytes())
    }

    /// Convert to hex string for debugging
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Create from hex string
    pub fn from_hex(hex: &str) -> Result<Self, RuntimeError> {
        let bytes = hex::decode(hex).map_err(|e| RuntimeError::ReplayStoreError {
            message: format!("Invalid hex fingerprint: {}", e),
        })?;

        if bytes.len() != 32 {
            return Err(RuntimeError::ReplayStoreError {
                message: "Fingerprint must be 32 bytes".to_string(),
            });
        }

        let mut array = [0u8; 32];
        array.copy_from_slice(&bytes);
        Ok(RequestFingerprint(array))
    }
}

fn normalize_serde_for_fingerprint<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
}

impl FileReplayStore {
    /// Create a new file-based replay store
    pub fn new(base_path: PathBuf) -> Result<Self, RuntimeError> {
        std::fs::create_dir_all(&base_path).map_err(|e| RuntimeError::ReplayStoreError {
            message: format!("Failed to create replay store directory: {}", e),
        })?;

        Ok(Self { base_path })
    }

    /// Get the file path for a fingerprint
    fn entry_path(&self, fingerprint: &RequestFingerprint) -> PathBuf {
        self.base_path
            .join(format!("{}.json", fingerprint.to_hex()))
    }
}

impl ReplayStore for FileReplayStore {
    fn store(
        &mut self,
        fingerprint: RequestFingerprint,
        entry: ReplayEntry,
    ) -> Result<(), RuntimeError> {
        let path = self.entry_path(&fingerprint);
        let content = serde_json::to_string_pretty(&entry)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    fn lookup(
        &self,
        fingerprint: &RequestFingerprint,
    ) -> Result<Option<ReplayEntry>, RuntimeError> {
        let path = self.entry_path(fingerprint);

        if !path.exists() {
            return Ok(None);
        }

        let content = std::fs::read_to_string(path)?;
        let entry: ReplayEntry = serde_json::from_str(&content)?;
        Ok(Some(entry))
    }

    fn list_fingerprints(&self) -> Result<Vec<RequestFingerprint>, RuntimeError> {
        let mut fingerprints = Vec::new();

        for entry in std::fs::read_dir(&self.base_path)? {
            let entry = entry?;
            let path = entry.path();

            if let Some(filename) = path.file_stem() {
                if let Some(hex) = filename.to_str() {
                    if let Ok(fingerprint) = RequestFingerprint::from_hex(hex) {
                        fingerprints.push(fingerprint);
                    }
                }
            }
        }

        Ok(fingerprints)
    }

    fn clear(&mut self) -> Result<(), RuntimeError> {
        for entry in std::fs::read_dir(&self.base_path)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension() == Some(std::ffi::OsStr::new("json")) {
                std::fs::remove_file(path)?;
            }
        }

        Ok(())
    }
}

impl ReplayStore for MemoryReplayStore {
    fn store(
        &mut self,
        fingerprint: RequestFingerprint,
        entry: ReplayEntry,
    ) -> Result<(), RuntimeError> {
        self.entries.insert(fingerprint, entry);
        Ok(())
    }

    fn lookup(
        &self,
        fingerprint: &RequestFingerprint,
    ) -> Result<Option<ReplayEntry>, RuntimeError> {
        Ok(self.entries.get(fingerprint).cloned())
    }

    fn list_fingerprints(&self) -> Result<Vec<RequestFingerprint>, RuntimeError> {
        Ok(self.entries.keys().cloned().collect())
    }

    fn clear(&mut self) -> Result<(), RuntimeError> {
        self.entries.clear();
        Ok(())
    }
}

/// Normalize JSON for consistent fingerprinting
fn normalize_json_for_fingerprint(value: &Value) -> String {
    // Convert to serde_json::Value first for consistent serialization
    let json_value = value_to_json_value(value);

    // Serialize with consistent formatting (no pretty printing, sorted keys)
    serde_json::to_string(&json_value).unwrap_or_else(|_| "null".to_string())
}

/// Convert plasm_core::Value to serde_json::Value
fn value_to_json_value(value: &Value) -> serde_json::Value {
    match value {
        Value::PlasmInputRef(_) => serde_json::to_value(value).unwrap_or(serde_json::Value::Null),
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Integer(i) => serde_json::Value::Number((*i).into()),
        Value::Float(n) => serde_json::Number::from_f64(*n)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::String(s) => serde_json::Value::String(s.clone()),
        Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(value_to_json_value).collect())
        }
        Value::Object(obj) => {
            let mut map = serde_json::Map::new();
            for (k, v) in obj {
                map.insert(k.clone(), value_to_json_value(v));
            }
            serde_json::Value::Object(map)
        }
        Value::UnionCtor { .. } => serde_json::to_value(value).unwrap_or(serde_json::Value::Null),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_json_abi::{Event, Function};
    use alloy_primitives::Address;
    use indexmap::IndexMap;
    use plasm_compile::{
        CompiledEvmCall, CompiledEvmLogs, EvmFieldSource, HttpBodyFormat, HttpMethod,
    };
    use plasm_core::Value;
    use tempfile::tempdir;

    fn create_test_request() -> CompiledOperation {
        CompiledOperation::Http(CompiledRequest {
            method: HttpMethod::Post,
            path: "/test/path".to_string(),
            query: None,
            body: Some(Value::Object({
                let mut obj = indexmap::IndexMap::new();
                obj.insert("key".to_string(), Value::String("value".to_string()));
                obj
            })),
            body_format: HttpBodyFormat::Json,
            multipart: None,
            headers: None,
        })
    }

    fn create_test_entry() -> ReplayEntry {
        ReplayEntry {
            request: create_test_request(),
            response: serde_json::json!({"result": "success"}),
            decoded_entities: vec![],
            schema_snapshot: serde_json::json!({}),
            timestamp: 1234567890,
        }
    }

    #[test]
    fn test_fingerprint_from_request() {
        let request = create_test_request();
        let fingerprint = RequestFingerprint::from_operation(&request);

        // Same request should produce same fingerprint
        let fingerprint2 = RequestFingerprint::from_operation(&request);
        assert_eq!(fingerprint, fingerprint2);
    }

    #[test]
    fn test_fingerprint_hex_conversion() {
        let request = create_test_request();
        let fingerprint = RequestFingerprint::from_operation(&request);

        let hex = fingerprint.to_hex();
        let parsed = RequestFingerprint::from_hex(&hex).unwrap();

        assert_eq!(fingerprint, parsed);
    }

    #[test]
    fn test_memory_store() {
        let mut store = MemoryReplayStore::default();
        let request = create_test_request();
        let fingerprint = RequestFingerprint::from_operation(&request);
        let entry = create_test_entry();

        // Store entry
        store.store(fingerprint.clone(), entry.clone()).unwrap();

        // Lookup entry
        let retrieved = store.lookup(&fingerprint).unwrap();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().timestamp, entry.timestamp);

        // List fingerprints
        let fingerprints = store.list_fingerprints().unwrap();
        assert_eq!(fingerprints.len(), 1);
        assert_eq!(fingerprints[0], fingerprint);
    }

    #[test]
    fn test_file_store() -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempdir()?;
        let mut store = FileReplayStore::new(temp_dir.path().to_path_buf())?;

        let request = create_test_request();
        let fingerprint = RequestFingerprint::from_operation(&request);
        let entry = create_test_entry();

        // Store entry
        store.store(fingerprint.clone(), entry.clone())?;

        // Lookup entry
        let retrieved = store.lookup(&fingerprint)?;
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().timestamp, entry.timestamp);

        // List fingerprints
        let fingerprints = store.list_fingerprints()?;
        assert_eq!(fingerprints.len(), 1);
        assert_eq!(fingerprints[0], fingerprint);

        Ok(())
    }

    #[test]
    fn test_normalize_json() {
        let mut obj = indexmap::IndexMap::new();
        obj.insert("b".to_string(), Value::Integer(2));
        obj.insert("a".to_string(), Value::Integer(1));

        let value = Value::Object(obj);
        let normalized = normalize_json_for_fingerprint(&value);

        // Should be valid JSON
        let parsed: serde_json::Value = serde_json::from_str(&normalized).unwrap();
        assert!(parsed.is_object());
    }

    #[test]
    fn test_different_requests_different_fingerprints() {
        let request1 = CompiledRequest {
            method: HttpMethod::Get,
            path: "/path1".to_string(),
            query: None,
            body: None,
            body_format: HttpBodyFormat::Json,
            multipart: None,
            headers: None,
        };

        let request2 = CompiledRequest {
            method: HttpMethod::Get,
            path: "/path2".to_string(),
            query: None,
            body: None,
            body_format: HttpBodyFormat::Json,
            multipart: None,
            headers: None,
        };

        let fp1 = RequestFingerprint::from_request(&request1);
        let fp2 = RequestFingerprint::from_request(&request2);

        assert_ne!(fp1, fp2);
    }

    #[test]
    fn test_evm_call_decode_affects_fingerprint() {
        let function = "function balanceOf(address owner) view returns (uint256 balance)"
            .parse::<Function>()
            .unwrap();
        let contract = "0x0000000000000000000000000000000000000001"
            .parse::<Address>()
            .unwrap();

        let mut decode_a = IndexMap::new();
        decode_a.insert("account".to_string(), EvmFieldSource::Input { index: 0 });
        decode_a.insert("balance".to_string(), EvmFieldSource::Output { index: 0 });

        let mut decode_b = IndexMap::new();
        decode_b.insert("balance".to_string(), EvmFieldSource::Output { index: 0 });

        let op_a = CompiledOperation::EvmCall(CompiledEvmCall {
            chain: 1u64.into(),
            contract,
            function: function.clone(),
            args: vec![Value::String(
                "0x00000000000000000000000000000000000000aa".to_string(),
            )],
            block: None,
            decode: decode_a,
        });
        let op_b = CompiledOperation::EvmCall(CompiledEvmCall {
            chain: 1u64.into(),
            contract,
            function,
            args: vec![Value::String(
                "0x00000000000000000000000000000000000000aa".to_string(),
            )],
            block: None,
            decode: decode_b,
        });

        assert_ne!(
            RequestFingerprint::from_operation(&op_a),
            RequestFingerprint::from_operation(&op_b)
        );
    }

    #[test]
    fn test_evm_logs_decode_affects_fingerprint() {
        let event = "event Transfer(address indexed from, address indexed to, uint256 value)"
            .parse::<Event>()
            .unwrap();
        let contract = "0x0000000000000000000000000000000000000001"
            .parse::<Address>()
            .unwrap();

        let mut decode_a = IndexMap::new();
        decode_a.insert(
            "event_id".to_string(),
            EvmFieldSource::LogMeta {
                key: plasm_compile::EvmLogMetaKey::EventId,
            },
        );
        decode_a.insert("to".to_string(), EvmFieldSource::Topic { index: 1 });

        let mut decode_b = IndexMap::new();
        decode_b.insert("to".to_string(), EvmFieldSource::Topic { index: 1 });

        let op_a = CompiledOperation::EvmLogs(Box::new(CompiledEvmLogs {
            chain: 1u64.into(),
            contract,
            event: event.clone(),
            indexed_filters: vec![None, None],
            from_block: Some(0),
            to_block: Some(100),
            pagination: None,
            decode: decode_a,
        }));
        let op_b = CompiledOperation::EvmLogs(Box::new(CompiledEvmLogs {
            chain: 1u64.into(),
            contract,
            event,
            indexed_filters: vec![None, None],
            from_block: Some(0),
            to_block: Some(100),
            pagination: None,
            decode: decode_b,
        }));

        assert_ne!(
            RequestFingerprint::from_operation(&op_a),
            RequestFingerprint::from_operation(&op_b)
        );
    }

    /// Typed IR stores predicate RHS as [`TypedComparisonValue`] but replay hashing uses normalized JSON bytes from [`Value`].
    #[test]
    fn typed_comparison_wire_preserves_http_fingerprint() {
        use plasm_core::typed_literal::TypedComparisonValue;

        let raw = Value::Integer(42);
        let roundtrip = TypedComparisonValue::from_value(raw.clone()).to_value();

        let mk = |body: Option<Value>| {
            CompiledOperation::Http(CompiledRequest {
                method: HttpMethod::Post,
                path: "/api".into(),
                query: None,
                body,
                body_format: HttpBodyFormat::Json,
                multipart: None,
                headers: None,
            })
        };

        assert_eq!(
            RequestFingerprint::from_operation(&mk(Some(raw))),
            RequestFingerprint::from_operation(&mk(Some(roundtrip)))
        );
    }

    /// [`InvokeInputPayload::lift`] then [`InvokeInputPayload::to_value`] must match raw wire JSON for fingerprint stability.
    #[test]
    fn invoke_input_payload_lift_preserves_http_fingerprint() {
        use plasm_core::schema::{
            InputFieldSchema, InputFieldWire, InputType, NamedValueSchema, StringSemantics,
            ValueDomainKey, CGS,
        };
        use plasm_core::typed_invoke::InvokeInputPayload;
        use plasm_core::FieldType;

        let mut cgs = CGS::new();
        cgs.values.insert(
            "replay_title".into(),
            NamedValueSchema {
                description: String::new(),
                field_type: FieldType::String,
                value_format: None,
                allowed_values: None,
                string_semantics: Some(StringSemantics::Short),
                array_items: None,
            },
        );
        let input_type = InputType::Object {
            fields: vec![InputFieldSchema {
                name: "title".into(),
                wire: InputFieldWire::Registry(ValueDomainKey::new("replay_title").expect("key")),
                required: true,
                description: None,
                default: None,
                role: None,
                wire_json_path: None,
                wire_array_element_key: None,
            }],
            additional_fields: false,
        };

        let body = Value::Object({
            let mut m = IndexMap::new();
            m.insert("title".into(), Value::String("hello".into()));
            m
        });

        let lifted = InvokeInputPayload::lift(&body, &input_type, &cgs);

        let mk = |body: Option<Value>| {
            CompiledOperation::Http(CompiledRequest {
                method: HttpMethod::Post,
                path: "/typed-ir".into(),
                query: None,
                body,
                body_format: HttpBodyFormat::Json,
                multipart: None,
                headers: None,
            })
        };

        assert_eq!(
            RequestFingerprint::from_operation(&mk(Some(body.clone()))),
            RequestFingerprint::from_operation(&mk(Some(lifted.to_value())))
        );
    }
}
