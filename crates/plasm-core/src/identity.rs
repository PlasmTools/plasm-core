//! Identity types: distinct string newtypes for entity, capability, and id slots (avoid accidental cross-wiring).
//!
//! Use these for map keys and function parameters where the string is not arbitrary user data but a
//! stable name from the CGS or wire contract.

use serde::{Deserialize, Serialize};
use std::borrow::Borrow;
use std::ops::Deref;

macro_rules! string_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(s: impl Into<String>) -> Self {
                Self(s.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl Deref for $name {
            type Target = str;

            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl Borrow<str> for $name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_string())
            }
        }

        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool {
                self.0 == *other
            }
        }

        impl PartialEq<String> for $name {
            fn eq(&self, other: &String) -> bool {
                self.0 == *other
            }
        }

        impl From<&String> for $name {
            fn from(s: &String) -> Self {
                Self(s.clone())
            }
        }

        impl From<$name> for String {
            fn from(s: $name) -> String {
                s.into_inner()
            }
        }
    };
}

string_id! {
    /// Entity type name (e.g. `Pet`, `Account`).
    EntityName
}
string_id! {
    /// Stable instance id for an entity (string form).
    EntityId
}
string_id! {
    /// Capability operation name (e.g. `list_pets`, `update_account`).
    CapabilityName
}
string_id! {
    /// Query / search / invoke input parameter wire name (e.g. `calendar_id`, `block_id`).
    CapabilityParamName
}
string_id! {
    /// Entity field key on a parent resource (e.g. `id`, `owner`) — used in relation bindings.
    EntityFieldName
}
string_id! {
    /// Path segment after `Entity(id).` that selects an invoke/create/update/delete capability
    /// (hyphenated surface derived from [`CapabilitySchema`](crate::schema::CapabilitySchema) name).
    PathMethodSegment
}
string_id! {
    /// CGS relation / outbound edge name (e.g. `children`, `labels`).
    RelationName
}
string_id! {
    /// Registry / catalog row id (`entry_id` in YAML and HTTP/MCP).
    RegistryEntryId
}

impl From<&EntityName> for EntityName {
    fn from(s: &EntityName) -> Self {
        s.clone()
    }
}

impl From<&EntityId> for EntityId {
    fn from(s: &EntityId) -> Self {
        s.clone()
    }
}

impl From<&CapabilityName> for CapabilityName {
    fn from(s: &CapabilityName) -> Self {
        s.clone()
    }
}

impl From<&RelationName> for RelationName {
    fn from(s: &RelationName) -> Self {
        s.clone()
    }
}

impl From<&RegistryEntryId> for RegistryEntryId {
    fn from(s: &RegistryEntryId) -> Self {
        s.clone()
    }
}
