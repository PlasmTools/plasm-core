use crate::error::CmlError;
use indexmap::IndexMap;
use plasm_core::{TypedFieldValue, Value};
use serde::{Deserialize, Serialize};

/// CML Expression - the typed mapping language
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CmlExpr {
    /// Variable reference
    #[serde(rename = "var")]
    Var { name: String },

    /// Constant value (algebraic [`TypedFieldValue`] — serializes like [`Value`] JSON).
    #[serde(rename = "const")]
    Const { value: TypedFieldValue },

    /// Object construction
    #[serde(rename = "object")]
    Object { fields: Vec<(String, CmlExpr)> },

    /// Array construction
    #[serde(rename = "array")]
    Array { elements: Vec<CmlExpr> },

    /// Conditional expression
    #[serde(rename = "if")]
    If {
        condition: Box<CmlCond>,
        then_expr: Box<CmlExpr>,
        else_expr: Box<CmlExpr>,
    },

    /// Join an array variable into a delimited string.
    ///
    /// Evaluates `expr` (which must yield `Value::Array`), converts each element
    /// to a string, then joins with `sep`.
    ///
    /// Use for CSV (`?genres=1,2,3`) or pipe-delimited (`?ids=1|2|3`) serialisation.
    /// For repeated-key arrays (`?embed=a&embed=b`), use a plain `Var` and let the
    /// HTTP execution layer expand the array automatically.
    ///
    /// # YAML
    /// ```yaml
    /// type: join
    /// sep: ","
    /// expr:
    ///   type: var
    ///   name: genres
    /// ```
    #[serde(rename = "join")]
    Join { sep: String, expr: Box<CmlExpr> },

    /// Format a string template using named placeholders.
    ///
    /// Placeholders use `{name}` syntax and must be provided in `vars`.
    /// Extra `vars` keys are rejected to keep mappings deterministic.
    ///
    /// # YAML
    /// ```yaml
    /// type: format
    /// template: "List(urn%3Ali%3Aperson%3A{id})"
    /// vars:
    ///   id:
    ///     type: var
    ///     name: member
    /// ```
    #[serde(rename = "format")]
    Format {
        template: String,
        vars: IndexMap<String, CmlExpr>,
    },
    /// Gmail `users.messages.send` JSON body: evaluates to `{ raw, threadId? }` from env keys
    /// `from`, `to`, `subject`, `plainBody` (required) and optional `threadId`, `inReplyTo`, `references`.
    #[serde(rename = "gmail_rfc5322_send_body")]
    GmailRfc5322SendBody {},
    /// Same wire shape as [`CmlExpr::GmailRfc5322SendBody`], but derives defaults from preflight
    /// keys `parent_*` (see `invoke_preflight` / `message_reply`). User keys `from`, `plainBody`
    /// required; optional `to`, `subject` override reply defaults.
    #[serde(rename = "gmail_rfc5322_reply_send_body")]
    GmailRfc5322ReplySendBody {},
}

/// CML Condition for if expressions
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CmlCond {
    /// Check if variable exists in environment
    #[serde(rename = "exists")]
    Exists { var: String },

    /// Equality check
    #[serde(rename = "equals")]
    Equals {
        left: Box<CmlExpr>,
        right: Box<CmlExpr>,
    },

    /// Boolean variable
    #[serde(rename = "bool")]
    Bool { expr: Box<CmlExpr> },
}

/// HTTP Method enum
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Head,
    Options,
}

/// How the runtime serializes an HTTP request body when `body` is present.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum HttpBodyFormat {
    /// `application/json` (default).
    #[default]
    Json,
    /// `application/x-www-form-urlencoded` from a flat `Value::Object` of scalars.
    FormUrlencoded,
    /// RFC 7578 `multipart/form-data` built from [`MultipartBodySpec`] / [`CompiledMultipartBody`].
    ///
    /// Requires [`CmlRequest::multipart`] with at least one part after evaluation (parts whose
    /// `content` evaluates to [`Value::Null`] are omitted). Do not set `body` on the same request.
    /// File bytes use a [`Value`] shaped like `{"__plasm_attachment": {"bytes_base64": "…", …}}`
    /// (same reserved key as CGS `blob` / HTTP decode); attachment-only `uri` without bytes is
    /// rejected at runtime.
    Multipart,
}

/// One part of a multipart request: evaluated `content` plus wire metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MultipartPartSpec {
    /// Form field name (`Content-Disposition` `name=`).
    pub name: String,
    /// Optional filename for file parts (`filename=`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
    /// Optional MIME type for this part (e.g. `image/png`, `application/json`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    pub content: CmlExpr,
}

/// Declarative multipart body: each part is a separate CML expression (same evaluation rules as `body:`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MultipartBodySpec {
    #[serde(default)]
    pub parts: Vec<MultipartPartSpec>,
}

/// Evaluated multipart payload for [`CompiledRequest`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompiledMultipartBody {
    pub parts: Vec<CompiledMultipartPart>,
}

/// One evaluated multipart part.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompiledMultipartPart {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    pub content: Value,
}

/// Path segment in URL
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PathSegment {
    #[serde(rename = "literal")]
    Literal { value: String },
    /// Interpolates a CML env variable. Optional `suffix` is appended with no `/` separator
    /// (Google APIs use `{range}:clear`, `{spreadsheetId}:batchUpdate`, etc.).
    #[serde(rename = "var")]
    Var {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        suffix: Option<String>,
    },
}

/// Pagination mapping for a query capability.
///
/// Parameters are declared as a map from API param name to advance strategy:
/// ```yaml
/// pagination:
///   params:
///     page:     {counter: 0}         # integer counter, starts at 0
///     per_page: {fixed: 30}          # constant sent on every request
///     cursor:   {from_response: next_cursor}  # extracted from each response
///   stop_when: {field: last_page, eq: true}   # optional explicit stop
/// ```
/// When `stop_when` is absent, pagination stops when a `FromResponse` param becomes
/// absent/null (cursor exhausted) or the items array is shorter than requested.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaginationConfig {
    /// Parameters to inject into each request. Keys are the API parameter names.
    #[serde(default)]
    pub params: indexmap::IndexMap<String, PaginationParam>,
    /// Where the pagination params are injected. Default: query string.
    #[serde(default)]
    pub location: PaginationLocation,
    /// When `location` is [`PaginationLocation::Body`], merge pagination params into this
    /// nested path inside the JSON body (e.g. GraphQL `variables.o.paginate` →
    /// `body_merge_path: [variables, o, paginate]` with params `page` / `limit`).
    /// When absent, params are merged at the top level of the body object (legacy HTTP JSON APIs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_merge_path: Option<Vec<String>>,
    /// JSON object path (from the root response body) where [`PaginationStop`] and
    /// [`PaginationParam::FromResponse`] read **relative field names**.
    /// Example (Relay): `[data, issues, pageInfo]` with `from_response: endCursor` and
    /// `stop_when: { field: hasNextPage, eq: false }`.
    /// When absent, those fields are read from the **top-level** response object (legacy HTTP).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_prefix: Option<Vec<String>>,
    /// When to stop paginating. When absent: stop when the items array is empty
    /// (short-page heuristic — last page has fewer items than requested).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_when: Option<PaginationStop>,
}

/// How a single pagination parameter advances across pages.
///
/// Serde-untagged: the YAML variant is inferred from the value shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PaginationParam {
    /// Integer counter: starts at `counter`, increments by `step` (default 1) per page.
    /// YAML: `page: {counter: 0}` or `offset: {counter: 0, step: 20}`.
    Counter {
        counter: i64,
        #[serde(default = "one_i64")]
        step: i64,
    },
    /// Fixed value: sent unchanged on every page request.
    /// YAML: `limit: {fixed: 20}` or `page_size: {fixed: 100}`.
    Fixed { fixed: serde_json::Value },
    /// Extracted from the previous response: absent on the first request; populated
    /// from `response[from_response]` on subsequent pages (or from the object at
    /// [`PaginationConfig::response_prefix`] when set). When the field is absent
    /// or null in the response, pagination stops (implicit stop condition).
    /// YAML: `start_cursor: {from_response: next_cursor}`.
    FromResponse { from_response: String },
}

fn one_i64() -> i64 {
    1
}

impl PaginationParam {
    /// Returns the starting value for this param on page 0 (None for FromResponse,
    /// which is absent until the first response is received).
    pub fn initial_value(&self) -> Option<serde_json::Value> {
        match self {
            PaginationParam::Counter { counter, .. } => {
                Some(serde_json::Value::Number((*counter).into()))
            }
            PaginationParam::Fixed { fixed } => Some(fixed.clone()),
            PaginationParam::FromResponse { .. } => None,
        }
    }

    /// Returns the default page size if this param represents a fixed page size.
    pub fn fixed_as_u32(&self) -> Option<u32> {
        if let PaginationParam::Fixed { fixed } = self {
            fixed.as_u64().map(|n| n as u32)
        } else {
            None
        }
    }
}

/// Where pagination params are injected in the outgoing request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PaginationLocation {
    /// Params appended to the URL query string (default).
    #[default]
    Query,
    /// Params merged into the JSON request body (for POST-paginated APIs).
    Body,
    /// No params needed — the next-page URL comes from the `Link: rel=next` header.
    LinkHeader,
    /// EVM block-range pagination: `from_block`/`to_block` injected per-page.
    BlockRange,
}

/// Declarative stop condition for pagination.
///
/// `#[serde(untagged)]` — the variant is inferred from the YAML key pattern:
/// - `{field: X, eq: Y}` → `FieldEquals`
/// - `{field: X, absent: true}` → `FieldAbsent`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PaginationStop {
    /// Stop when `response[field] == eq`. Handles both `has_more: false` and
    /// `last_page: true` patterns.
    /// YAML: `stop_when: {field: last_page, eq: true}`
    FieldEquals {
        field: String,
        eq: serde_json::Value,
    },
    /// Stop when `response[field]` is null or absent.
    /// YAML: `stop_when: {field: next, absent: true}`
    FieldAbsent { field: String, absent: bool },
}

/// Secondary GET merged into the primary JSON **before** `response_preprocess` / entity narrowing.
///
/// Used when one HTTP resource omits fields another sibling endpoint supplies (e.g. collab state
/// plus snapshot blocks).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuxiliaryHttpMerge {
    pub method: HttpMethod,
    pub path: Vec<PathSegment>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<CmlExpr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<CmlExpr>,
    /// Path from the auxiliary JSON root to the value copied onto the primary object (empty = whole body).
    #[serde(default)]
    pub from_path: Vec<String>,
    /// Top-level key set on the primary JSON object (insert or replace).
    pub into_key: String,
}

/// Optional decode hints for HTTP responses (non-paginated query/search and similar).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct HttpResponseDecode {
    /// JSON object key holding the array of entities (defaults to `results`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<String>,
    /// Path of object keys to the entity array (no trailing wildcard). When set, the decoder
    /// walks this path instead of a single top-level `items` key — e.g. NYT Article Search
    /// returns `{ "response": { "docs": [...] } }` as `items_path: [response, docs]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items_path: Option<Vec<String>>,
    /// After resolving the collection (path keys plus a trailing wildcard), drill into each
    /// element with this object key — e.g. Reddit `{ "data": { "children": [ { "kind": "t3", "data": { ... } } ] } }`
    /// uses `items_path: [data, children]` with `item_inner_key: data` so each row decodes from the inner `data` object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_inner_key: Option<String>,
    /// When true, the body is one JSON object (not an array); it is wrapped as
    /// `{ <items_key>: [ body ] }` before entity decoding.
    #[serde(default)]
    pub single: bool,
    /// When true, a root JSON **number** or **string** body is wrapped as `{ <items_key>: [ body ] }`
    /// so collection decoding can treat it as a one-row list (e.g. HN `maxitem.json`).
    #[serde(default)]
    pub wrap_root_scalar: bool,
    /// Optional **single** JSON response reshape before `items` / `items_path` decode.
    /// Mutually exclusive steps (one `kind` per mapping). In-place vs replace-root behavior
    /// is documented on each [`ResponsePreprocess`] variant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_preprocess: Option<ResponsePreprocess>,
    /// Optional follow-up HTTP GET evaluated with the **same** [`CmlEnv`] as the primary request;
    /// `from_path` on that JSON is merged into the primary body at `into_key` before narrowing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auxiliary_merge: Option<AuxiliaryHttpMerge>,
}

/// One optional HTTP JSON reshape before collection decode (`HttpResponseDecode::items` / `items_path`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResponsePreprocess {
    /// **Replace** the decode root: walk `path` to an array of objects, find the first where
    /// `id_field` matches the CML env `id_var`, then take `nested_array` (must be a JSON array)
    /// and re-wrap as `{ <items key>: that array }`. If `id_var` is missing, or `path` cannot
    /// be walked, the body is **unchanged**. If no element matches, yields an **empty** array
    /// under the items key.
    ArrayFindPluck {
        path: Vec<String>,
        id_field: String,
        id_var: String,
        nested_array: String,
    },
    /// **Replace** the decode root: walk `path` to an array of objects, concatenate each
    /// `from_each` array in order, re-wrap as `{ <items key>: concatenated }`. If `path` is
    /// invalid, the body is **unchanged**.
    ConcatFieldArrays {
        path: Vec<String>,
        from_each: String,
    },
    /// **In-place** at `path`: replace a JSON `string[]` with `[{ field: s }, …]`. Non-strings
    /// are **dropped**. If `path` is invalid, the body is **unchanged**.
    StringIdsToFieldObjects { path: Vec<String>, field: String },
}

impl CmlRequest {
    /// Key used to find the collection in the JSON body (default `results`).
    pub fn response_items_key(&self) -> &str {
        self.response
            .as_ref()
            .and_then(|r| r.items.as_deref())
            .filter(|s| !s.is_empty())
            .unwrap_or("results")
    }

    /// Whether the endpoint returns a single object instead of `{ items: [...] }`.
    pub fn response_is_single_object(&self) -> bool {
        self.response.as_ref().map(|r| r.single).unwrap_or(false)
    }
}

/// Legacy `response:` YAML shorthands (`single`, `results_list`, …) before structured
/// [`HttpResponseDecode`].
fn legacy_http_response_decode(s: &str) -> Result<HttpResponseDecode, String> {
    match s {
        "single" => Ok(HttpResponseDecode {
            items: None,
            items_path: None,
            item_inner_key: None,
            single: true,
            wrap_root_scalar: false,
            response_preprocess: None,
            auxiliary_merge: None,
        }),
        "results_list" => Ok(HttpResponseDecode {
            items: None,
            items_path: None,
            item_inner_key: None,
            single: false,
            wrap_root_scalar: false,
            response_preprocess: None,
            auxiliary_merge: None,
        }),
        "items_list" => Ok(HttpResponseDecode {
            items: Some("items".to_string()),
            items_path: None,
            item_inner_key: None,
            single: false,
            wrap_root_scalar: false,
            response_preprocess: None,
            auxiliary_merge: None,
        }),
        "bare_list" => Ok(HttpResponseDecode {
            items: None,
            items_path: None,
            item_inner_key: None,
            single: false,
            wrap_root_scalar: false,
            response_preprocess: None,
            auxiliary_merge: None,
        }),
        _ => Err(format!("unknown legacy response hint: {s}")),
    }
}

fn deserialize_optional_http_response_decode<'de, D>(
    deserializer: D,
) -> Result<Option<HttpResponseDecode>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Wrapper {
        Str(String),
        Decode(HttpResponseDecode),
    }

    let opt = Option::<Wrapper>::deserialize(deserializer)?;
    match opt {
        None => Ok(None),
        Some(Wrapper::Str(s)) => {
            let d = legacy_http_response_decode(&s).map_err(serde::de::Error::custom)?;
            Ok(Some(d))
        }
        Some(Wrapper::Decode(d)) => Ok(Some(d)),
    }
}

/// Complete HTTP request specification in CML
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CmlRequest {
    pub method: HttpMethod,
    pub path: Vec<PathSegment>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<CmlExpr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<CmlExpr>,
    /// When set with `body`, controls request serialization (default JSON).
    #[serde(default)]
    pub body_format: HttpBodyFormat,
    /// Multipart parts when [`HttpBodyFormat::Multipart`]; must be absent for JSON / form bodies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multipart: Option<MultipartBodySpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<CmlExpr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pagination: Option<PaginationConfig>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_http_response_decode",
        skip_serializing_if = "Option::is_none"
    )]
    pub response: Option<HttpResponseDecode>,
}

/// Environment for CML evaluation
pub type CmlEnv = IndexMap<String, Value>;

/// Type information for CML type checking
#[derive(Debug, Clone, PartialEq)]
pub enum CmlType {
    Any,
    Null,
    Bool,
    Number,
    String,
    Array(Box<CmlType>),
    Object(IndexMap<String, CmlType>),
}

impl CmlExpr {
    /// Create a variable reference
    pub fn var(name: impl Into<String>) -> Self {
        CmlExpr::Var { name: name.into() }
    }

    /// Create a constant
    pub fn const_(value: impl Into<TypedFieldValue>) -> Self {
        CmlExpr::Const {
            value: value.into(),
        }
    }

    /// Create an object
    pub fn object(fields: Vec<(String, CmlExpr)>) -> Self {
        CmlExpr::Object { fields }
    }

    /// Create an array
    pub fn array(elements: Vec<CmlExpr>) -> Self {
        CmlExpr::Array { elements }
    }

    /// Create a conditional
    pub fn if_(condition: CmlCond, then_expr: CmlExpr, else_expr: CmlExpr) -> Self {
        CmlExpr::If {
            condition: Box::new(condition),
            then_expr: Box::new(then_expr),
            else_expr: Box::new(else_expr),
        }
    }
}

impl CmlCond {
    /// Create an exists condition
    pub fn exists(var: impl Into<String>) -> Self {
        CmlCond::Exists { var: var.into() }
    }

    /// Create an equality condition
    pub fn equals(left: CmlExpr, right: CmlExpr) -> Self {
        CmlCond::Equals {
            left: Box::new(left),
            right: Box::new(right),
        }
    }

    /// Create a boolean condition
    pub fn bool(expr: CmlExpr) -> Self {
        CmlCond::Bool {
            expr: Box::new(expr),
        }
    }
}

impl PathSegment {
    /// Create a literal path segment
    pub fn literal(value: impl Into<String>) -> Self {
        PathSegment::Literal {
            value: value.into(),
        }
    }

    /// Create a variable path segment
    pub fn var(name: impl Into<String>) -> Self {
        PathSegment::Var {
            name: name.into(),
            suffix: None,
        }
    }

    /// Variable segment with a literal suffix (no extra path slash before the suffix).
    pub fn var_with_suffix(name: impl Into<String>, suffix: impl Into<String>) -> Self {
        PathSegment::Var {
            name: name.into(),
            suffix: Some(suffix.into()),
        }
    }
}

/// Path segment variable names in order (CML `type: var` only).
pub fn path_var_names_from_request(req: &CmlRequest) -> Vec<String> {
    req.path
        .iter()
        .filter_map(|seg| match seg {
            PathSegment::Var { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect()
}

impl CmlRequest {
    /// Create a new CML request
    pub fn new(method: HttpMethod, path: Vec<PathSegment>) -> Self {
        Self {
            method,
            path,
            query: None,
            body: None,
            body_format: HttpBodyFormat::Json,
            multipart: None,
            headers: None,
            pagination: None,
            response: None,
        }
    }

    /// Add query parameters
    pub fn with_query(mut self, query: CmlExpr) -> Self {
        self.query = Some(query);
        self
    }

    /// Add request body
    pub fn with_body(mut self, body: CmlExpr) -> Self {
        self.body = Some(body);
        self
    }

    /// Add headers
    pub fn with_headers(mut self, headers: CmlExpr) -> Self {
        self.headers = Some(headers);
        self
    }
}

/// Evaluate a CML expression in the given environment
pub fn eval_cml(expr: &CmlExpr, env: &CmlEnv) -> Result<Value, CmlError> {
    match expr {
        CmlExpr::Var { name } => env
            .get(name)
            .cloned()
            .ok_or_else(|| CmlError::VariableNotFound { name: name.clone() }),

        CmlExpr::Const { value } => Ok(value.to_value()),

        CmlExpr::Object { fields } => {
            let mut obj = IndexMap::new();
            for (key, expr) in fields {
                let value = eval_cml(expr, env)?;
                // Omit keys whose value is JSON-null: optional GraphQL/REST fields are often
                // authored as `if: exists` / `else: const null`; skipping here matches
                // "omit field = leave unchanged" and aligns compiled bodies with wire JSON
                // (see also `strip_null_fields` in plasm-runtime HTTP transport).
                if value != Value::Null {
                    obj.insert(key.clone(), value);
                }
            }
            Ok(Value::Object(obj))
        }

        CmlExpr::Array { elements } => {
            let values = elements
                .iter()
                .map(|expr| eval_cml(expr, env))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::Array(values))
        }

        CmlExpr::If {
            condition,
            then_expr,
            else_expr,
        } => {
            if eval_cond(condition, env)? {
                eval_cml(then_expr, env)
            } else {
                eval_cml(else_expr, env)
            }
        }

        CmlExpr::Join { sep, expr } => {
            let val = eval_cml(expr, env)?;
            match val {
                Value::Array(arr) => {
                    let joined = arr
                        .iter()
                        .map(|v| match v {
                            Value::String(s) => s.clone(),
                            Value::Integer(i) => i.to_string(),
                            Value::Float(f) => f.to_string(),
                            Value::Bool(b) => b.to_string(),
                            other => format!("{:?}", other),
                        })
                        .collect::<Vec<_>>()
                        .join(sep);
                    Ok(Value::String(joined))
                }
                // Non-array: pass through unchanged (allows conditional join on optional params)
                other => Ok(other),
            }
        }
        CmlExpr::Format { template, vars } => {
            let placeholders = extract_placeholders(template)?;

            for key in vars.keys() {
                if !placeholders.contains(key) {
                    return Err(CmlError::InvalidTemplate {
                        message: format!("format var '{key}' is unused in template '{template}'"),
                    });
                }
            }

            let mut rendered = template.clone();
            for name in placeholders {
                let expr = vars.get(&name).ok_or_else(|| CmlError::InvalidTemplate {
                    message: format!("missing format var '{name}' for template '{template}'"),
                })?;
                let value = eval_cml(expr, env)?;
                let replacement = value_to_string(&value);
                rendered = rendered.replace(&format!("{{{name}}}"), &replacement);
            }
            Ok(Value::String(rendered))
        }
        CmlExpr::GmailRfc5322SendBody {} => {
            crate::gmail_send_body::eval_gmail_rfc5322_send_body(env)
        }
        CmlExpr::GmailRfc5322ReplySendBody {} => {
            crate::gmail_send_body::eval_gmail_rfc5322_reply_send_body(env)
        }
    }
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::PlasmInputRef(_) => format!("{value:?}"),
        Value::String(s) => s.clone(),
        Value::Integer(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".to_string(),
        Value::Array(_) | Value::Object(_) | Value::UnionCtor { .. } => format!("{:?}", value),
    }
}

fn extract_placeholders(template: &str) -> Result<Vec<String>, CmlError> {
    let mut placeholders = Vec::new();
    let chars: Vec<char> = template.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] == '{' {
            let start = i + 1;
            let mut j = start;
            while j < chars.len() && chars[j] != '}' {
                j += 1;
            }
            if j == chars.len() {
                return Err(CmlError::InvalidTemplate {
                    message: format!("unclosed placeholder in template '{template}'"),
                });
            }
            let name: String = chars[start..j].iter().collect();
            if name.is_empty() {
                return Err(CmlError::InvalidTemplate {
                    message: format!("empty placeholder in template '{template}'"),
                });
            }
            placeholders.push(name);
            i = j + 1;
            continue;
        }
        i += 1;
    }
    Ok(placeholders)
}

/// Evaluate a CML condition
pub fn eval_cond(cond: &CmlCond, env: &CmlEnv) -> Result<bool, CmlError> {
    match cond {
        CmlCond::Exists { var } => Ok(env.contains_key(var)),

        CmlCond::Equals { left, right } => {
            let left_val = eval_cml(left, env)?;
            let right_val = eval_cml(right, env)?;
            Ok(left_val == right_val)
        }

        CmlCond::Bool { expr } => {
            let value = eval_cml(expr, env)?;
            match value {
                Value::Bool(b) => Ok(b),
                Value::Null => Ok(false),
                _ => Err(CmlError::TypeError {
                    message: format!("Expected boolean, got {:?}", value.type_name()),
                }),
            }
        }
    }
}

/// Evaluate a path segment
pub fn eval_path_segment(segment: &PathSegment, env: &CmlEnv) -> Result<String, CmlError> {
    match segment {
        PathSegment::Literal { value } => Ok(value.clone()),
        PathSegment::Var { name, suffix } => {
            let value = env
                .get(name)
                .ok_or_else(|| CmlError::VariableNotFound { name: name.clone() })?;

            let mut s = match value {
                Value::String(s) => s.clone(),
                Value::Integer(i) => i.to_string(),
                Value::Float(f) => f.to_string(),
                _ => {
                    return Err(CmlError::TypeError {
                        message: format!("Path variable '{}' must be string or number", name),
                    });
                }
            };
            if let Some(tail) = suffix {
                s.push_str(tail);
            }
            Ok(s)
        }
    }
}

/// Compile a CML request to a concrete HTTP request specification
pub fn compile_request(request: &CmlRequest, env: &CmlEnv) -> Result<CompiledRequest, CmlError> {
    // Evaluate path
    let path_segments = request
        .path
        .iter()
        .map(|seg| eval_path_segment(seg, env))
        .collect::<Result<Vec<_>, _>>()?;

    let path = format!("/{}", path_segments.join("/"));

    // Evaluate optional components
    let query = if let Some(query_expr) = &request.query {
        Some(eval_cml(query_expr, env)?)
    } else {
        None
    };

    let multipart = match request.body_format {
        HttpBodyFormat::Multipart => {
            if request.body.is_some() {
                return Err(CmlError::InvalidTemplate {
                    message: "body_format multipart cannot be combined with `body`; use multipart.parts only"
                        .to_string(),
                });
            }
            let spec = request
                .multipart
                .as_ref()
                .ok_or_else(|| CmlError::InvalidTemplate {
                    message:
                        "body_format multipart requires `multipart:` with a non-empty `parts` list"
                            .to_string(),
                })?;
            if spec.parts.is_empty() {
                return Err(CmlError::InvalidTemplate {
                    message: "multipart.parts must contain at least one part".to_string(),
                });
            }
            let mut compiled_parts = Vec::with_capacity(spec.parts.len());
            for p in &spec.parts {
                if p.name.is_empty() {
                    return Err(CmlError::InvalidTemplate {
                        message: "multipart part `name` must be non-empty".to_string(),
                    });
                }
                let content = eval_cml(&p.content, env)?;
                if content == Value::Null {
                    continue;
                }
                compiled_parts.push(CompiledMultipartPart {
                    name: p.name.clone(),
                    file_name: p.file_name.clone(),
                    content_type: p.content_type.clone(),
                    content,
                });
            }
            if compiled_parts.is_empty() {
                return Err(CmlError::InvalidTemplate {
                    message:
                        "multipart request has no parts after evaluation (all parts were null)"
                            .to_string(),
                });
            }
            Some(CompiledMultipartBody {
                parts: compiled_parts,
            })
        }
        HttpBodyFormat::Json | HttpBodyFormat::FormUrlencoded => {
            if request.multipart.is_some() {
                return Err(CmlError::InvalidTemplate {
                    message: "`multipart` is only valid when body_format is multipart".to_string(),
                });
            }
            None
        }
    };

    let body = if request.body_format == HttpBodyFormat::Multipart {
        None
    } else if let Some(body_expr) = &request.body {
        Some(eval_cml(body_expr, env)?)
    } else {
        None
    };

    let headers = if let Some(headers_expr) = &request.headers {
        Some(eval_cml(headers_expr, env)?)
    } else {
        None
    };

    Ok(CompiledRequest {
        method: request.method.clone(),
        path,
        query,
        body,
        body_format: request.body_format,
        multipart,
        headers,
    })
}

/// A compiled HTTP request ready for execution
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompiledRequest {
    pub method: HttpMethod,
    pub path: String,
    pub query: Option<Value>,
    pub body: Option<Value>,
    #[serde(default)]
    pub body_format: HttpBodyFormat,
    /// Present when [`HttpBodyFormat::Multipart`] was compiled; absent on older replay payloads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multipart: Option<CompiledMultipartBody>,
    pub headers: Option<Value>,
}

impl CompiledRequest {
    /// Convert this to a JSON representation
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }

    /// Get the URL path
    pub fn url_path(&self) -> &str {
        &self.path
    }

    /// Get the HTTP method as a string
    pub fn method_str(&self) -> &'static str {
        match self.method {
            HttpMethod::Get => "GET",
            HttpMethod::Post => "POST",
            HttpMethod::Put => "PUT",
            HttpMethod::Patch => "PATCH",
            HttpMethod::Delete => "DELETE",
            HttpMethod::Head => "HEAD",
            HttpMethod::Options => "OPTIONS",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_eval_var() {
        let mut env = CmlEnv::new();
        env.insert("test".to_string(), Value::String("hello".to_string()));

        let expr = CmlExpr::var("test");
        let result = eval_cml(&expr, &env).unwrap();

        assert_eq!(result, Value::String("hello".to_string()));
    }

    #[test]
    fn test_eval_const() {
        let env = CmlEnv::new();
        let expr = CmlExpr::const_(42);
        let result = eval_cml(&expr, &env).unwrap();

        assert_eq!(result, Value::Integer(42));
    }

    #[test]
    fn test_eval_object() {
        let mut env = CmlEnv::new();
        env.insert("name".to_string(), Value::String("test".to_string()));

        let expr = CmlExpr::object(vec![
            ("key".to_string(), CmlExpr::var("name")),
            ("value".to_string(), CmlExpr::const_(123)),
        ]);

        let result = eval_cml(&expr, &env).unwrap();

        if let Value::Object(obj) = result {
            assert_eq!(obj.get("key"), Some(&Value::String("test".to_string())));
            assert_eq!(obj.get("value"), Some(&Value::Integer(123)));
        } else {
            panic!("Expected object");
        }
    }

    #[test]
    fn test_object_omits_null_optional_fields() {
        let mut env = CmlEnv::new();
        env.insert("title".to_string(), Value::String("x".to_string()));
        // state absent — optional branch yields null; key must not appear
        let expr = CmlExpr::object(vec![
            (
                "title".to_string(),
                CmlExpr::if_(
                    CmlCond::exists("title"),
                    CmlExpr::var("title"),
                    CmlExpr::const_(Value::Null),
                ),
            ),
            (
                "stateId".to_string(),
                CmlExpr::if_(
                    CmlCond::exists("state"),
                    CmlExpr::var("state"),
                    CmlExpr::const_(Value::Null),
                ),
            ),
        ]);
        let result = eval_cml(&expr, &env).unwrap();
        let Value::Object(obj) = result else {
            panic!("expected object");
        };
        assert_eq!(obj.get("title"), Some(&Value::String("x".to_string())));
        assert!(!obj.contains_key("stateId"));
    }

    #[test]
    fn test_eval_if_exists() {
        let mut env = CmlEnv::new();
        env.insert("filter".to_string(), Value::String("test".to_string()));

        let expr = CmlExpr::if_(
            CmlCond::exists("filter"),
            CmlExpr::var("filter"),
            CmlExpr::const_(Value::Object(IndexMap::new())),
        );

        let result = eval_cml(&expr, &env).unwrap();
        assert_eq!(result, Value::String("test".to_string()));
    }

    #[test]
    fn test_eval_if_not_exists() {
        let env = CmlEnv::new(); // Empty environment

        let expr = CmlExpr::if_(
            CmlCond::exists("filter"),
            CmlExpr::const_("exists"),
            CmlExpr::const_("not_exists"),
        );

        let result = eval_cml(&expr, &env).unwrap();
        assert_eq!(result, Value::String("not_exists".to_string()));
    }

    #[test]
    fn http_response_decode_wrap_root_scalar_json() {
        let decode: HttpResponseDecode =
            serde_json::from_value(serde_json::json!({ "wrap_root_scalar": true })).unwrap();
        assert!(decode.wrap_root_scalar);
    }

    #[test]
    fn http_response_decode_response_preprocess_tagged_json() {
        let decode: HttpResponseDecode = serde_json::from_value(serde_json::json!({
            "items": "intervals",
            "response_preprocess": {
                "kind": "concat_field_arrays",
                "path": ["data"],
                "from_each": "intervals"
            }
        }))
        .unwrap();
        assert_eq!(
            decode.response_preprocess,
            Some(ResponsePreprocess::ConcatFieldArrays {
                path: vec!["data".to_string()],
                from_each: "intervals".to_string(),
            })
        );
    }

    #[test]
    fn test_compile_request() {
        let mut env = CmlEnv::new();
        env.insert("db_id".to_string(), Value::String("abc123".to_string()));
        env.insert(
            "filter".to_string(),
            Value::String("test_filter".to_string()),
        );

        let request = CmlRequest::new(
            HttpMethod::Post,
            vec![
                PathSegment::literal("databases"),
                PathSegment::var("db_id"),
                PathSegment::literal("query"),
            ],
        )
        .with_body(CmlExpr::object(vec![(
            "filter".to_string(),
            CmlExpr::var("filter"),
        )]));

        let compiled = compile_request(&request, &env).unwrap();

        assert_eq!(compiled.method, HttpMethod::Post);
        assert_eq!(compiled.path, "/databases/abc123/query");
        assert!(compiled.body.is_some());

        if let Some(Value::Object(body)) = compiled.body {
            assert_eq!(
                body.get("filter"),
                Some(&Value::String("test_filter".to_string()))
            );
        } else {
            panic!("Expected object body");
        }
    }

    #[test]
    fn test_path_segment_number_var() {
        let mut env = CmlEnv::new();
        env.insert("id".to_string(), Value::Integer(123));

        let segment = PathSegment::var("id");
        let result = eval_path_segment(&segment, &env).unwrap();

        assert_eq!(result, "123");
    }

    #[test]
    fn test_path_segment_var_suffix_google_style() {
        let mut env = CmlEnv::new();
        env.insert(
            "range".to_string(),
            Value::String("Sheet1!A1:B2".to_string()),
        );

        let segment = PathSegment::Var {
            name: "range".into(),
            suffix: Some(":append".into()),
        };
        let result = eval_path_segment(&segment, &env).unwrap();
        assert_eq!(result, "Sheet1!A1:B2:append");

        let compiled = compile_request(
            &CmlRequest::new(
                HttpMethod::Post,
                vec![
                    PathSegment::literal("v4"),
                    PathSegment::literal("spreadsheets"),
                    PathSegment::var("id"),
                    PathSegment::literal("values"),
                    segment,
                ],
            ),
            &{
                let mut e = CmlEnv::new();
                e.insert("id".to_string(), Value::String("abc".into()));
                e.insert("range".to_string(), Value::String("A1".into()));
                e
            },
        )
        .unwrap();
        assert_eq!(compiled.path, "/v4/spreadsheets/abc/values/A1:append");
    }

    #[test]
    fn test_missing_variable_error() {
        let env = CmlEnv::new();
        let expr = CmlExpr::var("missing");
        let result = eval_cml(&expr, &env);

        assert!(result.is_err());
        if let Err(CmlError::VariableNotFound { name }) = result {
            assert_eq!(name, "missing");
        } else {
            panic!("Expected VariableNotFound error");
        }
    }

    #[test]
    fn test_eval_format_success() {
        let mut env = CmlEnv::new();
        env.insert(
            "member_id".to_string(),
            Value::String("8675309".to_string()),
        );
        let expr = CmlExpr::Format {
            template: "List(urn%3Ali%3Aperson%3A{id})".to_string(),
            vars: IndexMap::from([("id".to_string(), CmlExpr::var("member_id"))]),
        };
        let result = eval_cml(&expr, &env).unwrap();
        assert_eq!(
            result,
            Value::String("List(urn%3Ali%3Aperson%3A8675309)".to_string())
        );
    }

    #[test]
    fn test_eval_format_missing_var() {
        let env = CmlEnv::new();
        let expr = CmlExpr::Format {
            template: "hello-{name}".to_string(),
            vars: IndexMap::new(),
        };
        let result = eval_cml(&expr, &env);
        assert!(matches!(result, Err(CmlError::InvalidTemplate { .. })));
    }

    #[test]
    fn test_eval_format_unused_var_rejected() {
        let env = CmlEnv::new();
        let expr = CmlExpr::Format {
            template: "hello-{name}".to_string(),
            vars: IndexMap::from([("extra".to_string(), CmlExpr::const_("x"))]),
        };
        let result = eval_cml(&expr, &env);
        assert!(matches!(result, Err(CmlError::InvalidTemplate { .. })));
    }

    #[test]
    fn multipart_request_json_round_trip() {
        let v = serde_json::json!({
            "method": "POST",
            "path": [{"type": "literal", "value": "upload"}],
            "body_format": "multipart",
            "multipart": {
                "parts": [
                    {"name": "note", "content": {"type": "const", "value": "hello"}},
                    {"name": "file", "file_name": "a.bin", "content_type": "application/octet-stream",
                     "content": {"type": "var", "name": "file"}}
                ]
            }
        });
        let req: CmlRequest = serde_json::from_value(v).unwrap();
        assert_eq!(req.body_format, HttpBodyFormat::Multipart);
        let parts = req.multipart.as_ref().expect("multipart").parts.as_slice();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].name, "note");
    }

    #[test]
    fn compile_multipart_skips_null_part() {
        use plasm_core::PLASM_ATTACHMENT_KEY;

        let mut env = CmlEnv::new();
        env.insert(
            "file".to_string(),
            Value::Object({
                let mut inner = IndexMap::new();
                inner.insert(
                    "bytes_base64".to_string(),
                    Value::String("QUJD".to_string()),
                );
                inner.insert(
                    "mime_type".to_string(),
                    Value::String("application/octet-stream".to_string()),
                );
                let mut outer = IndexMap::new();
                outer.insert(PLASM_ATTACHMENT_KEY.to_string(), Value::Object(inner));
                outer
            }),
        );

        let request = CmlRequest::new(
            HttpMethod::Post,
            vec![PathSegment::literal("pet"), PathSegment::literal("upload")],
        );
        let request = CmlRequest {
            body_format: HttpBodyFormat::Multipart,
            multipart: Some(MultipartBodySpec {
                parts: vec![
                    MultipartPartSpec {
                        name: "meta".to_string(),
                        file_name: None,
                        content_type: None,
                        content: CmlExpr::const_(Value::Null),
                    },
                    MultipartPartSpec {
                        name: "file".to_string(),
                        file_name: Some("x.bin".to_string()),
                        content_type: None,
                        content: CmlExpr::var("file"),
                    },
                ],
            }),
            ..request
        };

        let compiled = compile_request(&request, &env).unwrap();
        assert!(compiled.body.is_none());
        let mp = compiled.multipart.as_ref().unwrap();
        assert_eq!(mp.parts.len(), 1);
        assert_eq!(mp.parts[0].name, "file");
    }
}
