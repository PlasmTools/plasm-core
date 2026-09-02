//! Kernel + profile value domains for CGS `values:` rows.
//!
//! Authoring `type:` is either a [`KernelKind`] name or a core [`ProfileId`] name.
//! Catalog authors may add [`Constraints`] only.

use crate::money::MoneyWireFormat;
use crate::value::{CompOp, FieldType, TemporalWireFormat, ValueWireFormat};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::net::{Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

/// Closed set of wire kernels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KernelKind {
    String,
    Integer,
    Number,
    Boolean,
    Array,
    Json,
    Blob,
    Money,
    EntityRef {
        #[serde(default)]
        entry_id: crate::identity::RegistryEntryId,
        target: crate::identity::EntityName,
    },
}

/// Core-owned trait pack on a kernel (parse/validate, gloss, operator overlay).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileId {
    Markdown,
    Document,
    #[serde(rename = "json_text")]
    JsonText,
    Html,
    Uuid,
    Email,
    Url,
    #[serde(rename = "http_url")]
    HttpUrl,
    Hostname,
    E164,
    Ipv4,
    Ipv6,
    Hex,
    Base64,
    #[serde(rename = "base64url")]
    Base64Url,
    Rfc3339,
    #[serde(rename = "iso8601_date")]
    Iso8601Date,
    #[serde(rename = "unix_ms")]
    UnixMs,
    #[serde(rename = "unix_sec")]
    UnixSec,
    Enum,
    #[serde(rename = "multi_enum")]
    MultiEnum,
}

impl ProfileId {
    pub fn from_type_name(s: &str) -> Option<Self> {
        Some(match s {
            "markdown" => Self::Markdown,
            "document" => Self::Document,
            "json_text" => Self::JsonText,
            "html" => Self::Html,
            "uuid" => Self::Uuid,
            "email" => Self::Email,
            "url" => Self::Url,
            "http_url" => Self::HttpUrl,
            "hostname" => Self::Hostname,
            "e164" => Self::E164,
            "ipv4" => Self::Ipv4,
            "ipv6" => Self::Ipv6,
            "hex" => Self::Hex,
            "base64" => Self::Base64,
            "base64url" => Self::Base64Url,
            "rfc3339" => Self::Rfc3339,
            "iso8601_date" => Self::Iso8601Date,
            "unix_ms" => Self::UnixMs,
            "unix_sec" => Self::UnixSec,
            "enum" => Self::Enum,
            "multi_enum" => Self::MultiEnum,
            _ => return None,
        })
    }

    pub fn type_name(self) -> &'static str {
        match self {
            Self::Markdown => "markdown",
            Self::Document => "document",
            Self::JsonText => "json_text",
            Self::Html => "html",
            Self::Uuid => "uuid",
            Self::Email => "email",
            Self::Url => "url",
            Self::HttpUrl => "http_url",
            Self::Hostname => "hostname",
            Self::E164 => "e164",
            Self::Ipv4 => "ipv4",
            Self::Ipv6 => "ipv6",
            Self::Hex => "hex",
            Self::Base64 => "base64",
            Self::Base64Url => "base64url",
            Self::Rfc3339 => "rfc3339",
            Self::Iso8601Date => "iso8601_date",
            Self::UnixMs => "unix_ms",
            Self::UnixSec => "unix_sec",
            Self::Enum => "enum",
            Self::MultiEnum => "multi_enum",
        }
    }

    pub fn is_temporal(self) -> bool {
        matches!(
            self,
            Self::Rfc3339 | Self::Iso8601Date | Self::UnixMs | Self::UnixSec
        )
    }

    pub fn is_presentation(self) -> bool {
        matches!(
            self,
            Self::Markdown | Self::Document | Self::JsonText | Self::Html
        )
    }

    /// True for presentation profiles (markdown, HTML, documents, JSON text).
    pub fn is_structured_or_multiline(self) -> bool {
        self.is_presentation()
    }

    pub fn is_canned_string(self) -> bool {
        matches!(
            self,
            Self::Uuid
                | Self::Email
                | Self::Url
                | Self::HttpUrl
                | Self::Hostname
                | Self::E164
                | Self::Ipv4
                | Self::Ipv6
                | Self::Hex
                | Self::Base64
                | Self::Base64Url
        )
    }
}

/// Author constraints on a `values:` row.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Constraints {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_length: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusive_min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclusive_max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multiple_of: Option<f64>,
}

impl Constraints {
    pub fn is_empty(&self) -> bool {
        self.min_length.is_none()
            && self.max_length.is_none()
            && self.pattern.is_none()
            && self.min.is_none()
            && self.max.is_none()
            && self.exclusive_min.is_none()
            && self.exclusive_max.is_none()
            && self.multiple_of.is_none()
    }
}

const MAX_PATTERN_BYTES: usize = 512;
/// Soft cap on compiled automata size (bytes) for author patterns.
const PATTERN_COMPILED_SIZE_LIMIT: usize = 1024 * 1024;

/// Compile an author `pattern:` with load-time bounds.
pub fn compile_pattern(pattern: &str) -> Result<regex::Regex, String> {
    if pattern.len() > MAX_PATTERN_BYTES {
        return Err(format!(
            "pattern exceeds {MAX_PATTERN_BYTES} bytes (got {})",
            pattern.len()
        ));
    }
    regex::RegexBuilder::new(pattern)
        .size_limit(PATTERN_COMPILED_SIZE_LIMIT)
        .build()
        .map_err(|e| format!("invalid pattern: {e}"))
}

/// Characters forbidden inside teaching gloss text (pair / token delimiters).
pub const ENUM_GLOSS_FORBIDDEN_CHARS: &[char] = &[';', '|', '=', '‖'];

/// Enum token membership plus optional English teaching glosses.
///
/// Serialized flattened onto [`ValueDomain`] as `"enum"` (token list) and optional
/// `"enum_glosses"` (map). Teaching glosses are part of catalog identity
/// (`catalog_cgs_hash_hex`); they do not affect wire validation beyond membership.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnumMembership {
    #[serde(rename = "enum")]
    tokens: Vec<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "enum_glosses"
    )]
    glosses: Option<IndexMap<String, String>>,
}

impl EnumMembership {
    /// Tokens only (no teaching glosses).
    pub fn tokens_only(tokens: Vec<String>) -> Self {
        Self {
            tokens,
            glosses: None,
        }
    }

    /// Tokens plus glosses; rejects empty tokens and gloss text containing
    /// [`ENUM_GLOSS_FORBIDDEN_CHARS`].
    pub fn try_new(
        tokens: Vec<String>,
        glosses: Option<IndexMap<String, String>>,
    ) -> Result<Self, String> {
        if tokens.is_empty() {
            return Err("enum membership requires at least one token".into());
        }
        let glosses = match glosses {
            None => None,
            Some(g) => {
                let mut cleaned = IndexMap::new();
                for (tok, raw) in g {
                    let gloss = raw.trim();
                    if gloss.is_empty() {
                        continue;
                    }
                    if let Some(bad) = gloss
                        .chars()
                        .find(|c| ENUM_GLOSS_FORBIDDEN_CHARS.contains(c))
                    {
                        return Err(format!(
                            "enum gloss for token '{tok}' must not contain '{bad}' (reserved teaching delimiter)"
                        ));
                    }
                    cleaned.insert(tok, gloss.to_string());
                }
                if cleaned.is_empty() {
                    None
                } else {
                    Some(cleaned)
                }
            }
        };
        Ok(Self { tokens, glosses })
    }

    pub fn tokens(&self) -> &[String] {
        &self.tokens
    }

    pub fn glosses(&self) -> Option<&IndexMap<String, String>> {
        self.glosses.as_ref()
    }

    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }
}

/// Resolved value domain for one `values:` row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValueDomain {
    pub kernel: KernelKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<ProfileId>,
    #[serde(default, skip_serializing_if = "Constraints::is_empty")]
    pub constraints: Constraints,
    /// Enum membership (`type: enum` / `multi_enum`) — flattened as `"enum"` + optional `"enum_glosses"`.
    #[serde(flatten, default, skip_serializing_if = "Option::is_none")]
    pub enum_membership: Option<EnumMembership>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    #[serde(skip)]
    pub pattern_re: Option<regex::Regex>,
}

impl PartialEq for ValueDomain {
    fn eq(&self, other: &Self) -> bool {
        self.kernel == other.kernel
            && self.profile == other.profile
            && self.constraints == other.constraints
            && self.enum_membership == other.enum_membership
            && self.currency == other.currency
            && self.constraints.pattern == other.constraints.pattern
    }
}

/// Parse `values:` `type:` into kernel + optional profile. Rejects retired names.
pub fn parse_type_name(
    type_str: &str,
    target: Option<&str>,
) -> Result<(KernelKind, Option<ProfileId>), String> {
    let t = type_str.trim();
    if t.is_empty() {
        return Err("missing `type`".into());
    }
    match t {
        "date" => {
            return Err(
                "type 'date' was removed; use a temporal profile (`rfc3339`, `iso8601_date`, `unix_ms`, or `unix_sec`)"
                    .into(),
            );
        }
        "select" => {
            return Err("type 'select' was removed; use `type: enum` with `enum: [...]`".into());
        }
        "multi_select" => {
            return Err(
                "type 'multi_select' was removed; use `type: multi_enum` with `enum: [...]`".into(),
            );
        }
        "datetime" => {
            return Err(
                "type 'datetime' was removed; use a temporal profile (`rfc3339`, `iso8601_date`, `unix_ms`, or `unix_sec`)"
                    .into(),
            );
        }
        _ => {}
    }

    if let Some(profile) = ProfileId::from_type_name(t) {
        let kernel = match profile {
            ProfileId::UnixMs | ProfileId::UnixSec => KernelKind::Integer,
            ProfileId::MultiEnum => KernelKind::String, // shape via FieldType::MultiSelect
            ProfileId::Enum
            | ProfileId::Rfc3339
            | ProfileId::Iso8601Date
            | ProfileId::Markdown
            | ProfileId::Document
            | ProfileId::JsonText
            | ProfileId::Html
            | ProfileId::Uuid
            | ProfileId::Email
            | ProfileId::Url
            | ProfileId::HttpUrl
            | ProfileId::Hostname
            | ProfileId::E164
            | ProfileId::Ipv4
            | ProfileId::Ipv6
            | ProfileId::Hex
            | ProfileId::Base64
            | ProfileId::Base64Url => KernelKind::String,
        };
        return Ok((kernel, Some(profile)));
    }

    match t {
        "string" | "str" => Ok((KernelKind::String, None)),
        "integer" | "int" => Ok((KernelKind::Integer, None)),
        "number" | "float" => Ok((KernelKind::Number, None)),
        "boolean" | "bool" => Ok((KernelKind::Boolean, None)),
        "array" => Ok((KernelKind::Array, None)),
        "json" => Ok((KernelKind::Json, None)),
        "blob" => Ok((KernelKind::Blob, None)),
        "money" => Ok((KernelKind::Money, None)),
        "entity_ref" => {
            let Some(tgt) = target.map(str::trim).filter(|s| !s.is_empty()) else {
                return Err("type 'entity_ref' requires `target:`".into());
            };
            Ok((
                KernelKind::EntityRef {
                    entry_id: crate::identity::RegistryEntryId::default(),
                    target: crate::identity::EntityName::from(tgt),
                },
                None,
            ))
        }
        other => Err(format!("unknown type '{other}'")),
    }
}

impl Default for ValueDomain {
    fn default() -> Self {
        Self {
            kernel: KernelKind::String,
            profile: None,
            constraints: Constraints::default(),
            enum_membership: None,
            currency: None,
            pattern_re: None,
        }
    }
}

impl ValueDomain {
    /// True for blobs and presentation string profiles (markdown, HTML, documents, JSON text).
    pub fn is_structured_or_multiline(&self) -> bool {
        matches!(self.kernel, KernelKind::Blob)
            || self.profile.is_some_and(|p| p.is_structured_or_multiline())
    }

    /// Synthesize a domain from legacy [`FieldType`] + format/profile.
    ///
    /// **Transitional:** prefer [`Self::parse_type_name`], [`Self::new`], or catalog loader
    /// resolution for production paths. Retained for unit/integration fixtures that still
    /// construct [`NamedValueSchema`] from the pre-consolidation shape.
    pub fn from_legacy(
        field_type: &FieldType,
        value_format: Option<&ValueWireFormat>,
        profile: Option<ProfileId>,
        allowed_values: Option<Vec<String>>,
        currency: Option<String>,
    ) -> Self {
        let (kernel, profile) = match field_type {
            FieldType::Boolean => (KernelKind::Boolean, None),
            FieldType::Number => (KernelKind::Number, None),
            FieldType::Integer => (KernelKind::Integer, None),
            FieldType::Uuid => (KernelKind::String, Some(ProfileId::Uuid)),
            FieldType::Blob => (KernelKind::Blob, None),
            FieldType::String => (KernelKind::String, profile),
            FieldType::Select => (KernelKind::String, Some(ProfileId::Enum)),
            FieldType::MultiSelect => (KernelKind::String, Some(ProfileId::MultiEnum)),
            FieldType::Date => {
                let profile = match value_format {
                    Some(ValueWireFormat::Temporal(TemporalWireFormat::Rfc3339)) => {
                        Some(ProfileId::Rfc3339)
                    }
                    Some(ValueWireFormat::Temporal(TemporalWireFormat::Iso8601Date)) => {
                        Some(ProfileId::Iso8601Date)
                    }
                    Some(ValueWireFormat::Temporal(TemporalWireFormat::UnixMs)) => {
                        Some(ProfileId::UnixMs)
                    }
                    Some(ValueWireFormat::Temporal(TemporalWireFormat::UnixSec)) => {
                        Some(ProfileId::UnixSec)
                    }
                    _ => Some(ProfileId::Rfc3339),
                };
                let kernel = match profile {
                    Some(ProfileId::UnixMs | ProfileId::UnixSec) => KernelKind::Integer,
                    _ => KernelKind::String,
                };
                (kernel, profile)
            }
            FieldType::Array => (KernelKind::Array, None),
            FieldType::Json => (KernelKind::Json, None),
            FieldType::Money => (KernelKind::Money, None),
            FieldType::EntityRef { entry_id, target } => (
                KernelKind::EntityRef {
                    entry_id: entry_id.clone(),
                    target: target.clone(),
                },
                None,
            ),
        };
        let enum_membership = allowed_values
            .filter(|v| !v.is_empty())
            .map(EnumMembership::tokens_only);
        Self {
            kernel,
            profile,
            constraints: Constraints::default(),
            enum_membership,
            currency,
            pattern_re: None,
        }
    }

    /// Build a domain and compile any author `pattern:`.
    pub fn new(
        kernel: KernelKind,
        profile: Option<ProfileId>,
        mut constraints: Constraints,
        enum_membership: Option<EnumMembership>,
        currency: Option<String>,
    ) -> Result<Self, String> {
        let pattern_re = match constraints.pattern.as_deref() {
            Some(p) if !p.is_empty() => Some(compile_pattern(p)?),
            _ => {
                constraints.pattern = None;
                None
            }
        };
        Ok(Self {
            kernel,
            profile,
            constraints,
            enum_membership,
            currency,
            pattern_re,
        })
    }

    /// Enum tokens when this domain is `enum` / `multi_enum`.
    pub fn enum_tokens(&self) -> Option<&[String]> {
        self.enum_membership
            .as_ref()
            .map(EnumMembership::tokens)
            .filter(|t| !t.is_empty())
    }

    /// Effective legacy [`FieldType`] for shape / operator call sites.
    pub fn to_field_type(&self) -> FieldType {
        match (&self.kernel, self.profile) {
            (KernelKind::EntityRef { entry_id, target }, _) => FieldType::EntityRef {
                entry_id: entry_id.clone(),
                target: target.clone(),
            },
            (KernelKind::Boolean, _) => FieldType::Boolean,
            (KernelKind::Number, _) => FieldType::Number,
            (KernelKind::Integer, Some(p)) if p.is_temporal() => FieldType::Date,
            (KernelKind::Integer, _) => FieldType::Integer,
            (KernelKind::Array, _) => FieldType::Array,
            (KernelKind::Json, _) => FieldType::Json,
            (KernelKind::Blob, _) => FieldType::Blob,
            (KernelKind::Money, _) => FieldType::Money,
            (KernelKind::String, Some(ProfileId::Uuid)) => FieldType::Uuid,
            (KernelKind::String, Some(ProfileId::Enum)) => FieldType::Select,
            (KernelKind::String, Some(ProfileId::MultiEnum)) => FieldType::MultiSelect,
            (KernelKind::String, Some(p)) if p.is_temporal() => FieldType::Date,
            (KernelKind::String, _) => FieldType::String,
        }
    }

    pub fn to_value_format(&self) -> Option<ValueWireFormat> {
        match self.profile {
            Some(ProfileId::Rfc3339) => {
                Some(ValueWireFormat::Temporal(TemporalWireFormat::Rfc3339))
            }
            Some(ProfileId::Iso8601Date) => {
                Some(ValueWireFormat::Temporal(TemporalWireFormat::Iso8601Date))
            }
            Some(ProfileId::UnixMs) => Some(ValueWireFormat::Temporal(TemporalWireFormat::UnixMs)),
            Some(ProfileId::UnixSec) => {
                Some(ValueWireFormat::Temporal(TemporalWireFormat::UnixSec))
            }
            _ => match self.kernel {
                KernelKind::Money => Some(ValueWireFormat::Money(MoneyWireFormat::DecimalString)),
                _ => None,
            },
        }
    }

    pub fn compatible_operators(&self) -> &'static [CompOp] {
        if self.profile.is_some_and(|p| p.is_temporal())
            || matches!(
                self.kernel,
                KernelKind::Number | KernelKind::Integer | KernelKind::Money
            )
        {
            return &[
                CompOp::Eq,
                CompOp::Neq,
                CompOp::Gt,
                CompOp::Lt,
                CompOp::Gte,
                CompOp::Lte,
                CompOp::Exists,
            ];
        }
        match self.profile {
            Some(ProfileId::Enum) => &[CompOp::Eq, CompOp::Neq, CompOp::In, CompOp::Exists],
            Some(ProfileId::MultiEnum) => &[CompOp::Contains, CompOp::In, CompOp::Exists],
            _ => match self.kernel {
                KernelKind::Boolean => &[CompOp::Eq, CompOp::Neq, CompOp::Exists],
                KernelKind::Array => &[CompOp::Contains, CompOp::In, CompOp::Exists],
                KernelKind::Json => &[CompOp::Contains, CompOp::Exists],
                KernelKind::EntityRef { .. } => &[CompOp::Eq, CompOp::Neq, CompOp::Exists],
                KernelKind::String | KernelKind::Blob => {
                    &[CompOp::Eq, CompOp::Neq, CompOp::Contains, CompOp::Exists]
                }
                KernelKind::Number | KernelKind::Integer | KernelKind::Money => unreachable!(),
            },
        }
    }

    /// Teaching / TSV type keyword (`type:` name).
    pub fn gloss_type_keyword(&self) -> &'static str {
        if let Some(p) = self.profile {
            return p.type_name();
        }
        match &self.kernel {
            KernelKind::String => "string",
            KernelKind::Integer => "integer",
            KernelKind::Number => "number",
            KernelKind::Boolean => "boolean",
            KernelKind::Array => "array",
            KernelKind::Json => "json",
            KernelKind::Blob => "blob",
            KernelKind::Money => "money",
            KernelKind::EntityRef { .. } => "entity_ref",
        }
    }

    /// Compact constraint suffix for teaching gloss (e.g. ` min=1 max=256`).
    pub fn constraint_gloss_suffix(&self) -> String {
        let mut parts = Vec::new();
        let c = &self.constraints;
        if let Some(n) = c.min_length {
            parts.push(format!("min_length={n}"));
        }
        if let Some(n) = c.max_length {
            parts.push(format!("max_length={n}"));
        }
        if c.pattern.is_some() {
            parts.push("pattern".into());
        }
        if let Some(n) = c.min {
            parts.push(format!("min={n}"));
        }
        if let Some(n) = c.max {
            parts.push(format!("max={n}"));
        }
        if let Some(n) = c.exclusive_min {
            parts.push(format!("exclusive_min={n}"));
        }
        if let Some(n) = c.exclusive_max {
            parts.push(format!("exclusive_max={n}"));
        }
        if let Some(n) = c.multiple_of {
            parts.push(format!("multiple_of={n}"));
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!(" {}", parts.join(" "))
        }
    }

    /// Validate a concrete string against profile + string constraints.
    pub fn validate_string_value(&self, s: &str) -> Result<(), String> {
        if let Some(p) = self.profile {
            if p.is_canned_string() || p == ProfileId::Uuid {
                validate_string_profile(p, s)?;
            }
        }
        validate_string_constraints(s, &self.constraints, self.pattern_re.as_ref())?;
        if matches!(self.profile, Some(ProfileId::Enum)) {
            let Some(allowed) = self.enum_tokens() else {
                return Err("enum profile requires `enum:` membership list".into());
            };
            if !allowed.iter().any(|a| a == s) {
                return Err(format!("value '{s}' is not in enum {allowed:?}"));
            }
        }
        Ok(())
    }

    /// Validate a numeric value against numeric constraints.
    pub fn validate_number_value(&self, n: f64) -> Result<(), String> {
        validate_number_constraints(n, &self.constraints)
    }
}

/// Profile-specific string validation (canned set).
pub fn validate_string_profile(profile: ProfileId, s: &str) -> Result<(), String> {
    match profile {
        ProfileId::Uuid => validate_uuid(s),
        ProfileId::Email => validate_email(s),
        ProfileId::Url => validate_url(s, false),
        ProfileId::HttpUrl => validate_url(s, true),
        ProfileId::Hostname => validate_hostname(s),
        ProfileId::E164 => validate_e164(s),
        ProfileId::Ipv4 => Ipv4Addr::from_str(s)
            .map(|_| ())
            .map_err(|_| format!("invalid ipv4 '{s}'")),
        ProfileId::Ipv6 => Ipv6Addr::from_str(s)
            .map(|_| ())
            .map_err(|_| format!("invalid ipv6 '{s}'")),
        ProfileId::Hex => {
            if s.is_empty() || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(format!("invalid hex '{s}'"));
            }
            Ok(())
        }
        ProfileId::Base64 => validate_base64(s, false),
        ProfileId::Base64Url => validate_base64(s, true),
        _ => Ok(()),
    }
}

fn validate_uuid(s: &str) -> Result<(), String> {
    let b = s.as_bytes();
    if b.len() != 36 {
        return Err(format!("invalid uuid '{s}'"));
    }
    for (i, &c) in b.iter().enumerate() {
        match i {
            8 | 13 | 18 | 23 => {
                if c != b'-' {
                    return Err(format!("invalid uuid '{s}'"));
                }
            }
            _ => {
                if !c.is_ascii_hexdigit() {
                    return Err(format!("invalid uuid '{s}'"));
                }
            }
        }
    }
    Ok(())
}

fn validate_email(s: &str) -> Result<(), String> {
    if s.is_empty() || s.contains(' ') || s.contains('\n') {
        return Err(format!("invalid email '{s}'"));
    }
    let Some((local, domain)) = s.split_once('@') else {
        return Err(format!("invalid email '{s}'"));
    };
    if local.is_empty() || domain.is_empty() || !domain.contains('.') {
        return Err(format!("invalid email '{s}'"));
    }
    Ok(())
}

fn validate_url(s: &str, http_only: bool) -> Result<(), String> {
    let parsed = url::Url::parse(s).map_err(|_| format!("invalid url '{s}'"))?;
    let scheme = parsed.scheme();
    let ok_scheme = if http_only {
        scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
    } else {
        scheme.eq_ignore_ascii_case("http")
            || scheme.eq_ignore_ascii_case("https")
            || scheme.eq_ignore_ascii_case("ftp")
    };
    if !ok_scheme {
        return Err(format!("invalid url '{s}'"));
    }
    match parsed.host() {
        Some(url::Host::Domain(d)) if !d.is_empty() => Ok(()),
        Some(url::Host::Ipv4(_) | url::Host::Ipv6(_)) => Ok(()),
        _ => Err(format!("invalid url '{s}'")),
    }
}

fn validate_hostname(s: &str) -> Result<(), String> {
    if s.is_empty() || s.len() > 253 {
        return Err(format!("invalid hostname '{s}'"));
    }
    if s.starts_with('.') || s.ends_with('.') || s.contains("..") {
        return Err(format!("invalid hostname '{s}'"));
    }
    for label in s.split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err(format!("invalid hostname '{s}'"));
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err(format!("invalid hostname '{s}'"));
        }
        if !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(format!("invalid hostname '{s}'"));
        }
    }
    Ok(())
}

fn validate_e164(s: &str) -> Result<(), String> {
    let digits = if let Some(rest) = s.strip_prefix('+') {
        rest
    } else {
        s
    };
    if digits.is_empty() || digits.len() > 15 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("invalid e164 '{s}'"));
    }
    Ok(())
}

fn validate_base64(s: &str, url_safe: bool) -> Result<(), String> {
    if s.is_empty() {
        return Err("invalid base64: empty".into());
    }
    let alphabet_ok = |c: u8| {
        c.is_ascii_alphanumeric()
            || if url_safe {
                c == b'-' || c == b'_'
            } else {
                c == b'+' || c == b'/'
            }
            || c == b'='
    };
    if !s.bytes().all(alphabet_ok) {
        return Err(format!("invalid base64 '{s}'"));
    }
    Ok(())
}

pub fn validate_string_constraints(
    s: &str,
    c: &Constraints,
    pattern_re: Option<&regex::Regex>,
) -> Result<(), String> {
    if let Some(min) = c.min_length {
        if s.chars().count() < min {
            return Err(format!("string shorter than min_length {min}"));
        }
    }
    if let Some(max) = c.max_length {
        if s.chars().count() > max {
            return Err(format!("string longer than max_length {max}"));
        }
    }
    if let Some(re) = pattern_re {
        if !re.is_match(s) {
            return Err("string does not match pattern".into());
        }
    } else if let Some(ref pat) = c.pattern {
        let re = compile_pattern(pat)?;
        if !re.is_match(s) {
            return Err(format!("string does not match pattern {pat:?}"));
        }
    }
    Ok(())
}

/// Validate string length / pattern constraints (compiles `pattern` when no prebuilt regex).
pub fn validate_constraints_on_string(constraints: &Constraints, s: &str) -> Result<(), String> {
    validate_string_constraints(s, constraints, None)
}

pub fn validate_number_constraints(n: f64, c: &Constraints) -> Result<(), String> {
    if let Some(min) = c.min {
        if n < min {
            return Err(format!("value {n} below min {min}"));
        }
    }
    if let Some(max) = c.max {
        if n > max {
            return Err(format!("value {n} above max {max}"));
        }
    }
    if let Some(xmin) = c.exclusive_min {
        if n <= xmin {
            return Err(format!("value {n} not above exclusive_min {xmin}"));
        }
    }
    if let Some(xmax) = c.exclusive_max {
        if n >= xmax {
            return Err(format!("value {n} not below exclusive_max {xmax}"));
        }
    }
    if let Some(m) = c.multiple_of {
        if m == 0.0 {
            return Err("multiple_of must be non-zero".into());
        }
        let q = n / m;
        if (q - q.round()).abs() > 1e-9 {
            return Err(format!("value {n} is not a multiple of {m}"));
        }
    }
    Ok(())
}

/// Validate numeric min/max / multiple_of constraints.
pub fn validate_constraints_on_number(constraints: &Constraints, n: f64) -> Result<(), String> {
    validate_number_constraints(n, constraints)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_retired_type_names() {
        assert!(parse_type_name("date", None)
            .unwrap_err()
            .contains("rfc3339"));
        assert!(parse_type_name("select", None)
            .unwrap_err()
            .contains("enum"));
        assert!(parse_type_name("multi_select", None)
            .unwrap_err()
            .contains("multi_enum"));
    }

    #[test]
    fn parses_profiles_and_kernels() {
        let (k, p) = parse_type_name("email", None).unwrap();
        assert!(matches!(k, KernelKind::String));
        assert_eq!(p, Some(ProfileId::Email));
        let (k, p) = parse_type_name("unix_ms", None).unwrap();
        assert!(matches!(k, KernelKind::Integer));
        assert_eq!(p, Some(ProfileId::UnixMs));
        let (k, p) = parse_type_name("string", None).unwrap();
        assert!(matches!(k, KernelKind::String));
        assert_eq!(p, None);
    }

    #[test]
    fn canned_validators() {
        validate_string_profile(ProfileId::Email, "a@b.co").unwrap();
        assert!(validate_string_profile(ProfileId::Email, "nope").is_err());
        validate_string_profile(ProfileId::Ipv4, "1.2.3.4").unwrap();
        assert!(validate_string_profile(ProfileId::Ipv4, "999.1.1.1").is_err());
        validate_string_profile(ProfileId::Uuid, "550e8400-e29b-41d4-a716-446655440000").unwrap();
        validate_string_profile(ProfileId::HttpUrl, "https://example.com/x").unwrap();
        assert!(validate_string_profile(ProfileId::HttpUrl, "ftp://example.com").is_err());
    }

    #[test]
    fn pattern_compile_bounds() {
        assert!(compile_pattern("^a+$").is_ok());
        let big = "a".repeat(MAX_PATTERN_BYTES + 1);
        assert!(compile_pattern(&big).unwrap_err().contains("exceeds"));
    }

    #[test]
    fn to_field_type_mapping() {
        let d = ValueDomain::new(
            KernelKind::String,
            Some(ProfileId::Rfc3339),
            Constraints::default(),
            None,
            None,
        )
        .unwrap();
        assert_eq!(d.to_field_type(), FieldType::Date);
        assert!(matches!(
            d.to_value_format(),
            Some(ValueWireFormat::Temporal(TemporalWireFormat::Rfc3339))
        ));
        assert!(d.compatible_operators().contains(&CompOp::Gt));
    }

    #[test]
    fn enum_membership_flattens_as_enum_and_enum_glosses_keys() {
        let mut glosses = IndexMap::new();
        glosses.insert("pending".into(), "awaiting settlement".into());
        let m = EnumMembership::try_new(vec!["pending".into(), "approved".into()], Some(glosses))
            .unwrap();
        let d = ValueDomain::new(
            KernelKind::String,
            Some(ProfileId::Enum),
            Constraints::default(),
            Some(m),
            None,
        )
        .unwrap();
        let v = serde_json::to_value(&d).unwrap();
        assert_eq!(
            v.get("enum"),
            Some(&serde_json::json!(["pending", "approved"]))
        );
        assert_eq!(
            v.get("enum_glosses"),
            Some(&serde_json::json!({"pending": "awaiting settlement"}))
        );
        assert!(v.get("enum_membership").is_none());
        let back: ValueDomain = serde_json::from_value(v).unwrap();
        assert_eq!(back.enum_tokens(), d.enum_tokens());
        assert_eq!(
            back.enum_membership.as_ref().and_then(|m| m.glosses()),
            d.enum_membership.as_ref().and_then(|m| m.glosses())
        );
    }

    #[test]
    fn enum_membership_rejects_reserved_gloss_delimiter() {
        let mut glosses = IndexMap::new();
        glosses.insert("a".into(), "x = y".into());
        let err = EnumMembership::try_new(vec!["a".into()], Some(glosses)).unwrap_err();
        assert!(err.contains("'='"), "{err}");
    }
}
