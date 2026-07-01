use serde::{Deserialize, Serialize};

macro_rules! plan_string_atom {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, String> {
                let value = value.into();
                if value.trim().is_empty() {
                    return Err(format!("{} must be non-empty", stringify!($name)));
                }
                if value.contains("[object Object]") {
                    return Err(format!(
                        "{} contains JavaScript object string coercion ([object Object])",
                        stringify!($name)
                    ));
                }
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    };
}

plan_string_atom! {
    /// Symbolic callback/item binding name.
    BindingName
}

plan_string_atom! {
    /// Named Plan return or synthetic result field.
    OutputName
}

/// Dotted field path after validation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FieldPath(Vec<String>);

impl FieldPath {
    pub fn new(segments: Vec<String>) -> Result<Self, String> {
        if segments.is_empty() || segments.iter().any(|s| s.trim().is_empty()) {
            return Err("FieldPath must contain non-empty segments".to_string());
        }
        Ok(Self(segments))
    }

    pub fn from_dotted(path: &str) -> Result<Self, String> {
        Self::new(path.split('.').map(str::to_string).collect())
    }

    pub fn segments(&self) -> &[String] {
        &self.0
    }

    pub fn dotted(&self) -> String {
        self.0.join(".")
    }
}

/// Qualified catalog entity key for dispatch (wire / plan serde shape).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanQualifiedEntityKey {
    pub entry_id: String,
    pub entity: String,
}

impl From<crate::QualifiedEntityKey> for PlanQualifiedEntityKey {
    fn from(q: crate::QualifiedEntityKey) -> Self {
        Self {
            entry_id: q.entry_id.into(),
            entity: q.entity.into(),
        }
    }
}

impl From<PlanQualifiedEntityKey> for crate::QualifiedEntityKey {
    fn from(q: PlanQualifiedEntityKey) -> Self {
        crate::QualifiedEntityKey::new(q.entry_id, q.entity)
    }
}
