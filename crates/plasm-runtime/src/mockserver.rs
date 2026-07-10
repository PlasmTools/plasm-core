use crate::RuntimeError;
use plasm_compile::{BackendFilter, BackendOp, CompiledRequest, HttpBodyFormat, HttpMethod};
use plasm_core::Value;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// MockServer expectation configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MockServerExpectation {
    pub request: MockServerRequest,
    pub response: MockServerResponse,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub times: Option<MockServerTimes>,
}

/// MockServer request matcher
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MockServerRequest {
    pub method: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query_string_parameters: Option<serde_json::Value>,
}

/// MockServer response specification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MockServerResponse {
    #[serde(rename = "statusCode")]
    pub status_code: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delay: Option<MockServerDelay>,
}

/// MockServer timing constraints
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MockServerTimes {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining_times: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unlimited: Option<bool>,
}

/// MockServer response delay
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MockServerDelay {
    #[serde(rename = "timeUnit")]
    pub time_unit: String, // "MILLISECONDS", "SECONDS", etc.
    pub value: u64,
}

/// MockServer client for managing expectations
pub struct MockServerClient {
    base_url: String,
    client: reqwest::Client,
}

impl MockServerClient {
    /// Create a new MockServer client
    pub fn new(base_url: String) -> Self {
        Self {
            base_url,
            client: reqwest::Client::new(),
        }
    }

    /// Set up an expectation on MockServer
    pub async fn create_expectation(
        &self,
        expectation: &MockServerExpectation,
    ) -> Result<(), RuntimeError> {
        let url = format!("{}/mockserver/expectation", self.base_url);

        let response = self.client.put(&url).json(expectation).send().await?;

        if !response.status().is_success() {
            return Err(RuntimeError::RequestError {
                message: format!(
                    "Failed to create MockServer expectation: {}",
                    response.status()
                ),
                attempts: 1,
                status: None,
                body: None,
            });
        }

        Ok(())
    }

    /// Clear all expectations
    pub async fn clear_expectations(&self) -> Result<(), RuntimeError> {
        let url = format!("{}/mockserver/clear", self.base_url);

        let response = self
            .client
            .put(&url)
            .json(&json!({ "type": "EXPECTATIONS" }))
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(RuntimeError::RequestError {
                message: format!(
                    "Failed to clear MockServer expectations: {}",
                    response.status()
                ),
                attempts: 1,
                status: None,
                body: None,
            });
        }

        Ok(())
    }

    /// Verify that a request was received
    pub async fn verify_request(&self, request: &MockServerRequest) -> Result<bool, RuntimeError> {
        let url = format!("{}/mockserver/verify", self.base_url);

        let verification = json!({
            "httpRequest": request
        });

        let response = self.client.put(&url).json(&verification).send().await?;

        Ok(response.status().is_success())
    }

    /// Reset MockServer state
    pub async fn reset(&self) -> Result<(), RuntimeError> {
        let url = format!("{}/mockserver/reset", self.base_url);

        let response = self.client.put(&url).send().await?;

        if !response.status().is_success() {
            return Err(RuntimeError::RequestError {
                message: format!("Failed to reset MockServer: {}", response.status()),
                attempts: 1,
                status: None,
                body: None,
            });
        }

        Ok(())
    }
}

/// Convert our CompiledRequest to MockServer request matcher
pub fn compiled_request_to_mockserver(request: &CompiledRequest) -> MockServerRequest {
    let method = match request.method {
        HttpMethod::Get => "GET",
        HttpMethod::Post => "POST",
        HttpMethod::Put => "PUT",
        HttpMethod::Patch => "PATCH",
        HttpMethod::Delete => "DELETE",
        HttpMethod::Head => "HEAD",
        HttpMethod::Options => "OPTIONS",
    };

    let body = if request.body_format == HttpBodyFormat::Multipart {
        request
            .multipart
            .as_ref()
            .map(|mp| serde_json::to_value(mp).unwrap_or(serde_json::Value::Null))
    } else {
        request.body.as_ref().map(plasm_value_to_json)
    };
    let query_params = request.query.as_ref().map(plasm_value_to_json);
    let headers = request.headers.as_ref().map(plasm_value_to_json);

    MockServerRequest {
        method: method.to_string(),
        path: request.path.clone(),
        body,
        headers,
        query_string_parameters: query_params,
    }
}

/// Generate MockServer expectations from our BackendFilter system
pub fn generate_mockserver_expectations(
    requests: Vec<CompiledRequest>,
    mock_entities: Vec<MockEntity>,
) -> Vec<MockServerExpectation> {
    let mut expectations = Vec::new();

    for (i, request) in requests.iter().enumerate() {
        let mock_request = compiled_request_to_mockserver(request);

        // Generate appropriate mock response based on entity data
        let response_body = if let Some(entity) = mock_entities.get(i) {
            Some(json!({
                "results": [entity.to_json()],
                "count": 1
            }))
        } else {
            Some(json!({
                "results": [],
                "count": 0
            }))
        };

        let expectation = MockServerExpectation {
            request: mock_request,
            response: MockServerResponse {
                status_code: 200,
                body: response_body,
                headers: Some(json!({
                    "Content-Type": ["application/json"]
                })),
                delay: None,
            },
            times: Some(MockServerTimes {
                remaining_times: None,
                unlimited: Some(true),
            }),
        };

        expectations.push(expectation);
    }

    expectations
}

/// A mock entity for generating test responses
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MockEntity {
    pub id: String,
    pub entity_type: String,
    pub fields: indexmap::IndexMap<String, Value>,
    pub relations: indexmap::IndexMap<String, Vec<String>>,
}

impl MockEntity {
    /// Create a new mock entity
    pub fn new(id: String, entity_type: String) -> Self {
        Self {
            id,
            entity_type,
            fields: indexmap::IndexMap::new(),
            relations: indexmap::IndexMap::new(),
        }
    }

    /// Add a field
    pub fn with_field(mut self, name: String, value: Value) -> Self {
        self.fields.insert(name, value);
        self
    }

    /// Add a relation
    pub fn with_relation(mut self, name: String, related_ids: Vec<String>) -> Self {
        self.relations.insert(name, related_ids);
        self
    }

    /// Convert to JSON for MockServer responses
    pub fn to_json(&self) -> serde_json::Value {
        let mut json = json!({
            "id": self.id,
            "_type": self.entity_type
        });

        // Add fields
        for (field, value) in &self.fields {
            json[field] = plasm_value_to_json(value);
        }

        // Add relations as reference arrays
        for (relation, refs) in &self.relations {
            json[relation] = json!(refs);
        }

        json
    }
}

/// Convert plasm_core::Value to serde_json::Value for MockServer
fn plasm_value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::PlasmInputRef(_) => serde_json::to_value(value).unwrap_or(serde_json::Value::Null),
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Integer(i) => serde_json::Value::Number((*i).into()),
        Value::Float(n) => serde_json::Number::from_f64(*n)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::String(s) | Value::PhraseIdent(s) => serde_json::Value::String(s.clone()),
        Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(plasm_value_to_json).collect())
        }
        Value::Object(obj) => {
            let mut map = serde_json::Map::new();
            for (k, v) in obj {
                map.insert(k.clone(), plasm_value_to_json(v));
            }
            serde_json::Value::Object(map)
        }
        Value::UnionCtor { .. } => serde_json::to_value(value).unwrap_or(serde_json::Value::Null),
    }
}

/// Create MockServer expectations that match our BackendFilter queries
pub fn create_filter_based_expectations(
    entity_type: &str,
    filter: &BackendFilter,
    mock_data: Vec<MockEntity>,
) -> MockServerExpectation {
    // Create request matcher for query endpoint
    let request_body = json!({
        "filter": backend_filter_to_json(filter)
    });

    let mock_request = MockServerRequest {
        method: "POST".to_string(),
        path: format!("/query/{}", entity_type),
        body: Some(request_body),
        headers: Some(json!({
            "Content-Type": ["application/json"]
        })),
        query_string_parameters: None,
    };

    // Filter mock data based on the filter
    let filtered_data: Vec<_> = mock_data
        .into_iter()
        .filter(|entity| entity_matches_filter(entity, filter))
        .collect();

    let response_body = json!({
        "results": filtered_data.iter().map(|e| e.to_json()).collect::<Vec<_>>(),
        "count": filtered_data.len()
    });

    MockServerExpectation {
        request: mock_request,
        response: MockServerResponse {
            status_code: 200,
            body: Some(response_body),
            headers: Some(json!({
                "Content-Type": ["application/json"]
            })),
            delay: None,
        },
        times: Some(MockServerTimes {
            remaining_times: None,
            unlimited: Some(true),
        }),
    }
}

/// Convert BackendFilter to JSON for MockServer request matching
fn backend_filter_to_json(filter: &BackendFilter) -> serde_json::Value {
    serde_json::to_value(filter).unwrap_or(json!({}))
}

/// Simple entity filtering for mock responses (simplified implementation)
fn entity_matches_filter(entity: &MockEntity, filter: &BackendFilter) -> bool {
    match filter {
        BackendFilter::True => true,
        BackendFilter::False => false,

        BackendFilter::Field {
            field,
            operator,
            value,
        } => {
            if let Some(entity_value) = entity.fields.get(field) {
                match operator {
                    BackendOp::Equals => entity_value == value,
                    BackendOp::NotEquals => entity_value != value,
                    BackendOp::GreaterThan => {
                        if let (Some(ev), Some(fv)) = (entity_value.as_number(), value.as_number())
                        {
                            ev > fv
                        } else {
                            false
                        }
                    }
                    // Add other operators as needed
                    _ => true, // Simplified for POC
                }
            } else {
                false
            }
        }

        BackendFilter::And { filters } => filters.iter().all(|f| entity_matches_filter(entity, f)),

        BackendFilter::Or { filters } => filters.iter().any(|f| entity_matches_filter(entity, f)),

        BackendFilter::Not { filter } => !entity_matches_filter(entity, filter),

        BackendFilter::Relation { .. } => {
            // Relation filtering would require more complex logic
            true // Simplified for POC
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_compile::HttpMethod;

    #[test]
    fn test_compiled_request_conversion() {
        let request = CompiledRequest {
            method: HttpMethod::Post,
            path: "/query/Account".to_string(),
            query: None,
            body: Some(Value::String("test".to_string())),
            body_format: HttpBodyFormat::Json,
            multipart: None,
            headers: None,
        };

        let mock_request = compiled_request_to_mockserver(&request);

        assert_eq!(mock_request.method, "POST");
        assert_eq!(mock_request.path, "/query/Account");
        assert!(mock_request.body.is_some());
    }

    #[test]
    fn test_mock_entity_creation() {
        let entity = MockEntity::new("test-1".to_string(), "Account".to_string())
            .with_field("name".to_string(), Value::String("Test Corp".to_string()))
            .with_field("revenue".to_string(), Value::Float(1000.0));

        let json = entity.to_json();
        assert_eq!(json["id"], "test-1");
        assert_eq!(json["name"], "Test Corp");
        assert_eq!(json["revenue"], 1000.0);
    }

    #[test]
    fn test_entity_filtering() {
        let entity = MockEntity::new("test-1".to_string(), "Account".to_string())
            .with_field("revenue".to_string(), Value::Float(1500.0));

        let filter = BackendFilter::field("revenue", BackendOp::GreaterThan, 1000.0);
        assert!(entity_matches_filter(&entity, &filter));

        let filter2 = BackendFilter::field("revenue", BackendOp::GreaterThan, 2000.0);
        assert!(!entity_matches_filter(&entity, &filter2));
    }

    #[test]
    fn test_create_filter_based_expectation() {
        let mock_data = vec![
            MockEntity::new("acc-1".to_string(), "Account".to_string())
                .with_field("region".to_string(), Value::String("EMEA".to_string())),
            MockEntity::new("acc-2".to_string(), "Account".to_string())
                .with_field("region".to_string(), Value::String("APAC".to_string())),
        ];

        let filter = BackendFilter::field("region", BackendOp::Equals, "EMEA");
        let expectation = create_filter_based_expectations("Account", &filter, mock_data);

        assert_eq!(expectation.request.method, "POST");
        assert_eq!(expectation.request.path, "/query/Account");
        assert_eq!(expectation.response.status_code, 200);

        // Should only return EMEA account
        if let Some(body) = &expectation.response.body {
            if let Some(results) = body.get("results") {
                if let Some(arr) = results.as_array() {
                    assert_eq!(arr.len(), 1);
                }
            }
        }
    }
}
