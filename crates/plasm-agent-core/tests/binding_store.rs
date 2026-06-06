//! Integration-style unit tests for scoped binding store helpers.

use indexmap::IndexMap;
use plasm_agent_core::binding_slots::{BindingScope, bindings_complete_for_entry};
use uuid::Uuid;

#[test]
fn bindings_complete_requires_all_wires_for_fibery() {
    let mut partial = IndexMap::new();
    assert!(!bindings_complete_for_entry("fibery", &partial));
    partial.insert("catalog_http_origin".into(), "https://acme.fibery.io".into());
    assert!(bindings_complete_for_entry("fibery", &partial));
}

#[test]
fn binding_scope_converts_to_envelope_scope_v1() {
    let scope = BindingScope::new("tenant-1", Uuid::nil(), "fibery");
    let v1: plasm_runtime::binding_kv::BindingScopeV1 = (&scope).into();
    assert_eq!(v1.tenant_id, "tenant-1");
    assert_eq!(v1.entry_id, "fibery");
}

#[test]
fn host_binding_wires_match_binding_slot() {
    use plasm_agent_core::binding_slots::BindingSlot;
    for slot in BindingSlot::all() {
        assert!(plasm_core::bind_wire_validate::is_known_host_binding_wire(
            slot.wire_name()
        ));
    }
}
