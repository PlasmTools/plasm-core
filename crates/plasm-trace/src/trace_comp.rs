//! Validated PlasmComp topology on code-plan trace rows (wire + optional dry-run extras).

use plasm_core::plasm_monad::PlasmComp;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Canonical code-plan topology on trace rows: validated [`PlasmComp`] plus optional wire extras.
#[derive(Clone, Debug, PartialEq)]
pub struct TraceCompWire {
    pub comp: PlasmComp,
    pub summary: Option<serde_json::Value>,
    pub returns: Vec<String>,
}

impl TraceCompWire {
    #[must_use]
    pub fn plan_display_name(&self) -> String {
        self.comp
            .name
            .clone()
            .unwrap_or_else(|| "unnamed plan".to_string())
    }

    #[must_use]
    pub fn node_count(&self) -> usize {
        self.comp.steps.len().max(self.comp.bind.topo.len())
    }

    pub fn validate(&self) -> Result<(), String> {
        self.comp.validate()
    }

    pub fn from_json_value(v: serde_json::Value) -> Result<Self, String> {
        let summary = v.get("summary").cloned();
        let returns = v
            .get("returns")
            .and_then(|r| serde_json::from_value(r.clone()).ok())
            .unwrap_or_default();
        let mut comp_value = v;
        if let Some(obj) = comp_value.as_object_mut() {
            obj.remove("summary");
            obj.remove("returns");
        }
        let comp: PlasmComp = serde_json::from_value(comp_value).map_err(|e| e.to_string())?;
        comp.validate()?;
        Ok(Self {
            comp,
            summary,
            returns,
        })
    }

    #[must_use]
    pub fn to_json_value(&self) -> serde_json::Value {
        let mut v = serde_json::to_value(&self.comp).expect("PlasmComp serializes");
        if let Some(obj) = v.as_object_mut() {
            if let Some(summary) = &self.summary {
                obj.insert("summary".into(), summary.clone());
            }
            if !self.returns.is_empty() {
                obj.insert("returns".into(), serde_json::json!(self.returns));
            }
        }
        v
    }
}

/// Minimal valid comp JSON for trace contract tests (shared with Elixir/JS fixtures).
#[must_use]
pub fn minimal_trace_comp_json() -> serde_json::Value {
    serde_json::from_str(include_str!("../../../fixtures/trace/minimal_comp.json"))
        .expect("minimal comp fixture JSON")
}

#[cfg(test)]
pub(crate) fn minimal_trace_comp_wire() -> TraceCompWire {
    TraceCompWire::from_json_value(minimal_trace_comp_json()).expect("minimal trace comp")
}

/// Serde adapter for `Arc<TraceCompWire>` on trace segments (orphan-safe).
pub mod trace_comp_arc {
    use super::TraceCompWire;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::sync::Arc;

    pub fn serialize<S>(arc: &Arc<TraceCompWire>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        arc.as_ref().serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Arc<TraceCompWire>, D::Error>
    where
        D: Deserializer<'de>,
    {
        TraceCompWire::deserialize(deserializer).map(Arc::new)
    }
}

impl Serialize for TraceCompWire {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.to_json_value().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TraceCompWire {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let v = serde_json::Value::deserialize(deserializer)?;
        Self::from_json_value(v).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_comp_without_bind_topo() {
        let v = serde_json::json!({
            "version": 1,
            "steps": {"n1": {"kind": "invoke", "plan_kind": "query", "effect_class": "read", "result_shape": "list"}},
            "bind": {"topo": [], "deps": {}},
            "return": {"kind": "step", "step": "n1"}
        });
        assert!(TraceCompWire::from_json_value(v).is_err());
    }

    #[test]
    fn strips_wire_extras_before_plasm_comp_deserialize() {
        let mut v = minimal_trace_comp_json();
        v.as_object_mut().unwrap().insert(
            "summary".into(),
            serde_json::json!({"nodes": 1}),
        );
        let wire = TraceCompWire::from_json_value(v).expect("valid comp");
        assert_eq!(wire.summary.as_ref().and_then(|s| s.get("nodes")), Some(&serde_json::json!(1)));
    }

    #[test]
    fn round_trips_valid_comp_with_summary() {
        let mut v = minimal_trace_comp_json();
        v.as_object_mut().unwrap().insert(
            "summary".into(),
            serde_json::json!({"nodes": 1}),
        );
        v.as_object_mut()
            .unwrap()
            .insert("returns".into(), serde_json::json!(["n1"]));
        let wire = TraceCompWire::from_json_value(v.clone()).expect("valid comp");
        assert_eq!(wire.plan_display_name(), "demo");
        assert_eq!(wire.node_count(), 1);
        assert_eq!(wire.to_json_value()["bind"]["topo"], serde_json::json!(["n1"]));
    }
}
