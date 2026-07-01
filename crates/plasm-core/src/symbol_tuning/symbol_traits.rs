//! Read-only resolver vs append-only allocator trait boundaries for session symbol tables.

use std::sync::Arc;

use crate::cgs_federation::CgsLayer;
use crate::identity::EntityFieldName;
use crate::schema::{CapabilitySchema, EntityDef, CGS};

use super::keys::CatalogScope;
use super::{
    EntityBinding, ExposedEntitySymbolRow, MethodBinding, RelationBinding, SlotBinding, SymbolMap,
    SymbolResolveError, TeachingExposureSession,
};

/// Typed resolution for parser, DAG, and compile-time symbol dispatch.
pub trait SymbolResolve: Send + Sync {
    fn resolve_session_entity(&self, token: &str) -> Result<EntityBinding, SymbolResolveError>;
    fn resolve_session_slot(&self, token: &str) -> Result<SlotBinding, SymbolResolveError>;
    fn resolve_session_method(&self, token: &str) -> Result<MethodBinding, SymbolResolveError>;
    fn resolve_session_method_for_invoke(
        &self,
        token: &str,
        anchor_entity: &str,
    ) -> Result<MethodBinding, SymbolResolveError>;
    fn resolve_session_relation(&self, token: &str) -> Result<RelationBinding, SymbolResolveError>;

    fn resolve_entity_field(
        &self,
        catalog: CatalogScope,
        entity: &str,
        ent: &EntityDef,
        token: &str,
    ) -> Result<String, SymbolResolveError>;

    fn resolve_compound_key(
        &self,
        catalog: CatalogScope,
        entity: &str,
        key_vars: &[EntityFieldName],
        raw_key: &str,
    ) -> Result<String, SymbolResolveError>;

    fn resolve_query_filter_field(
        &self,
        catalog: CatalogScope,
        entity: &str,
        ent: &EntityDef,
        cgs: &CGS,
        token: &str,
    ) -> Result<String, SymbolResolveError>;

    fn resolve_cap_param(
        &self,
        catalog: CatalogScope,
        domain: &str,
        capability: &str,
        token: &str,
        invoke_cap: &CapabilitySchema,
    ) -> Result<String, SymbolResolveError>;

    fn resolve_opaque_session_method_capability<'a>(
        &self,
        layers: &[CgsLayer<'a>],
        token: &str,
        anchor_entity: &str,
    ) -> Result<&'a CapabilitySchema, SymbolResolveError>;
}

/// Forward opaque-symbol render helpers (teaching table / qualified lookups).
pub trait SymbolRender: Send + Sync {
    fn resolve_binding_field_segment(&self, token: &str) -> String;
    fn resolve_session_entity_symbol(&self, token: &str) -> Option<String>;
    fn resolve_method_symbol_token(&self, label: &str) -> Option<&str>;
    fn resolve_method_symbol_triple(&self, label: &str) -> Option<(&str, &str, &str)>;

    fn entity_sym_for(&self, catalog_entry_id: &str, canonical: &str) -> String;
    fn method_sym_for(&self, catalog_entry_id: &str, domain: &str, capability: &str) -> String;
    fn ident_sym_entity_field_for(
        &self,
        catalog_entry_id: &str,
        entity: &str,
        field: &str,
    ) -> String;
    fn ident_sym_cap_param_for(
        &self,
        catalog_entry_id: &str,
        domain: &str,
        capability: &str,
        param: &str,
    ) -> String;
    fn cap_param_syms_hint(
        &self,
        catalog_entry_id: &str,
        domain: &str,
        capability: &str,
    ) -> String;

    fn ident_sym_relation_for(
        &self,
        catalog_entry_id: &str,
        source_entity: &str,
        relation: &str,
    ) -> String;

    fn entity_stamps_for_wire(&self, wire: &str) -> Vec<(String, String)>;
    fn exposed_entity_symbol_rows(&self) -> Vec<ExposedEntitySymbolRow>;
    fn entry_id_for_entity_symbol(&self, sym: &str) -> Option<String>;

    /// Single registry row for this map, when unambiguous (teaching-table parse alignment).
    fn sole_registry_entry_id(&self) -> Option<&str> {
        None
    }

    /// True when every exposed entity uses the unset forward-map key (`""`).
    fn is_unset_single_graph_session(&self) -> bool {
        false
    }

    /// Wire label → opaque symbol when unambiguous across session slots.
    fn ident_sym_unambiguous(&self, name: &str) -> Option<String> {
        let _ = name;
        None
    }

    /// Rewrite canonical entity/field tokens for LLM recovery hints.
    fn collapse_tokens_for_feedback(&self, input: &str) -> String {
        input.to_string()
    }
}

/// Full read-only session symbol surface for parser, DAG, and render consumers.
pub trait SymbolSession: SymbolResolve + SymbolRender {}

impl<T: SymbolResolve + SymbolRender + ?Sized> SymbolSession for T {}

impl SymbolResolve for SymbolMap {
    fn resolve_session_entity(&self, token: &str) -> Result<EntityBinding, SymbolResolveError> {
        SymbolMap::resolve_session_entity(self, token)
    }
    fn resolve_session_slot(&self, token: &str) -> Result<SlotBinding, SymbolResolveError> {
        SymbolMap::resolve_session_slot(self, token)
    }
    fn resolve_session_method(&self, token: &str) -> Result<MethodBinding, SymbolResolveError> {
        SymbolMap::resolve_session_method(self, token)
    }
    fn resolve_session_method_for_invoke(
        &self,
        token: &str,
        anchor_entity: &str,
    ) -> Result<MethodBinding, SymbolResolveError> {
        SymbolMap::resolve_session_method_for_invoke(self, token, anchor_entity)
    }
    fn resolve_session_relation(&self, token: &str) -> Result<RelationBinding, SymbolResolveError> {
        SymbolMap::resolve_session_relation(self, token)
    }
    fn resolve_entity_field(
        &self,
        catalog: CatalogScope,
        entity: &str,
        ent: &EntityDef,
        token: &str,
    ) -> Result<String, SymbolResolveError> {
        SymbolMap::resolve_entity_field(self, catalog, entity, ent, token)
    }
    fn resolve_compound_key(
        &self,
        catalog: CatalogScope,
        entity: &str,
        key_vars: &[EntityFieldName],
        raw_key: &str,
    ) -> Result<String, SymbolResolveError> {
        SymbolMap::resolve_compound_key(self, catalog, entity, key_vars, raw_key)
    }
    fn resolve_query_filter_field(
        &self,
        catalog: CatalogScope,
        entity: &str,
        ent: &EntityDef,
        cgs: &CGS,
        token: &str,
    ) -> Result<String, SymbolResolveError> {
        SymbolMap::resolve_query_filter_field(self, catalog, entity, ent, cgs, token)
    }
    fn resolve_cap_param(
        &self,
        catalog: CatalogScope,
        domain: &str,
        capability: &str,
        token: &str,
        invoke_cap: &CapabilitySchema,
    ) -> Result<String, SymbolResolveError> {
        SymbolMap::resolve_cap_param(
            self,
            catalog,
            domain,
            capability,
            token,
            invoke_cap,
        )
    }
    fn resolve_opaque_session_method_capability<'a>(
        &self,
        layers: &[CgsLayer<'a>],
        token: &str,
        anchor_entity: &str,
    ) -> Result<&'a CapabilitySchema, SymbolResolveError> {
        SymbolMap::resolve_opaque_session_method_capability(self, layers, token, anchor_entity)
    }
}

impl SymbolRender for SymbolMap {
    fn resolve_binding_field_segment(&self, token: &str) -> String {
        SymbolMap::resolve_binding_field_segment(self, token)
    }
    fn resolve_session_entity_symbol(&self, token: &str) -> Option<String> {
        SymbolMap::resolve_session_entity_symbol(self, token)
    }
    fn resolve_method_symbol_token(&self, label: &str) -> Option<&str> {
        SymbolMap::resolve_method_symbol_token(self, label)
    }
    fn resolve_method_symbol_triple(&self, label: &str) -> Option<(&str, &str, &str)> {
        SymbolMap::resolve_method_symbol_triple(self, label)
    }
    fn entity_sym_for(&self, catalog_entry_id: &str, entity: &str) -> String {
        SymbolMap::entity_sym_for(self, catalog_entry_id, entity)
    }
    fn method_sym_for(&self, catalog_entry_id: &str, domain: &str, capability: &str) -> String {
        SymbolMap::method_sym_for(self, catalog_entry_id, domain, capability)
    }
    fn ident_sym_entity_field_for(
        &self,
        catalog_entry_id: &str,
        entity: &str,
        field: &str,
    ) -> String {
        SymbolMap::ident_sym_entity_field_for(self, catalog_entry_id, entity, field)
    }
    fn ident_sym_cap_param_for(
        &self,
        catalog_entry_id: &str,
        domain: &str,
        capability: &str,
        param: &str,
    ) -> String {
        SymbolMap::ident_sym_cap_param_for(self, catalog_entry_id, domain, capability, param)
    }
    fn cap_param_syms_hint(
        &self,
        catalog_entry_id: &str,
        domain: &str,
        capability: &str,
    ) -> String {
        SymbolMap::cap_param_syms_hint(self, catalog_entry_id, domain, capability)
    }
    fn ident_sym_relation_for(
        &self,
        catalog_entry_id: &str,
        source_entity: &str,
        relation: &str,
    ) -> String {
        SymbolMap::ident_sym_relation_for(self, catalog_entry_id, source_entity, relation)
    }
    fn entity_stamps_for_wire(&self, wire: &str) -> Vec<(String, String)> {
        SymbolMap::entity_stamps_for_wire(self, wire)
    }
    fn exposed_entity_symbol_rows(&self) -> Vec<ExposedEntitySymbolRow> {
        SymbolMap::exposed_entity_symbol_rows(self)
    }
    fn entry_id_for_entity_symbol(&self, sym: &str) -> Option<String> {
        SymbolMap::entry_id_for_entity_symbol(self, sym)
    }
    fn sole_registry_entry_id(&self) -> Option<&str> {
        SymbolMap::sole_registry_entry_id(self)
    }
    fn is_unset_single_graph_session(&self) -> bool {
        SymbolMap::is_unset_single_graph_session(self)
    }
    fn ident_sym_unambiguous(&self, name: &str) -> Option<String> {
        SymbolMap::ident_sym_unambiguous(self, name)
    }
    fn collapse_tokens_for_feedback(&self, input: &str) -> String {
        SymbolMap::collapse_tokens_for_feedback(self, input)
    }
}

macro_rules! delegate_symbol_resolve_deref {
    () => {
        impl<T: SymbolResolve + ?Sized> SymbolResolve for &T {
            fn resolve_session_entity(
                &self,
                token: &str,
            ) -> Result<EntityBinding, SymbolResolveError> {
                SymbolResolve::resolve_session_entity(*self, token)
            }
            fn resolve_session_slot(
                &self,
                token: &str,
            ) -> Result<SlotBinding, SymbolResolveError> {
                SymbolResolve::resolve_session_slot(*self, token)
            }
            fn resolve_session_method(
                &self,
                token: &str,
            ) -> Result<MethodBinding, SymbolResolveError> {
                SymbolResolve::resolve_session_method(*self, token)
            }
            fn resolve_session_method_for_invoke(
                &self,
                token: &str,
                anchor_entity: &str,
            ) -> Result<MethodBinding, SymbolResolveError> {
                SymbolResolve::resolve_session_method_for_invoke(*self, token, anchor_entity)
            }
            fn resolve_session_relation(
                &self,
                token: &str,
            ) -> Result<RelationBinding, SymbolResolveError> {
                SymbolResolve::resolve_session_relation(*self, token)
            }
            fn resolve_entity_field(
                &self,
                catalog: CatalogScope<'_>,
                entity: &str,
                ent: &EntityDef,
                token: &str,
            ) -> Result<String, SymbolResolveError> {
                SymbolResolve::resolve_entity_field(*self, catalog, entity, ent, token)
            }
            fn resolve_compound_key(
                &self,
                catalog: CatalogScope<'_>,
                entity: &str,
                key_vars: &[EntityFieldName],
                raw_key: &str,
            ) -> Result<String, SymbolResolveError> {
                SymbolResolve::resolve_compound_key(*self, catalog, entity, key_vars, raw_key)
            }
            fn resolve_query_filter_field(
                &self,
                catalog: CatalogScope<'_>,
                entity: &str,
                ent: &EntityDef,
                cgs: &CGS,
                token: &str,
            ) -> Result<String, SymbolResolveError> {
                SymbolResolve::resolve_query_filter_field(*self, catalog, entity, ent, cgs, token)
            }
            fn resolve_cap_param(
                &self,
                catalog: CatalogScope<'_>,
                domain: &str,
                capability: &str,
                token: &str,
                invoke_cap: &CapabilitySchema,
            ) -> Result<String, SymbolResolveError> {
                SymbolResolve::resolve_cap_param(
                    *self,
                    catalog,
                    domain,
                    capability,
                    token,
                    invoke_cap,
                )
            }
            fn resolve_opaque_session_method_capability<'a>(
                &self,
                layers: &[CgsLayer<'a>],
                token: &str,
                anchor_entity: &str,
            ) -> Result<&'a CapabilitySchema, SymbolResolveError> {
                SymbolResolve::resolve_opaque_session_method_capability(
                    *self,
                    layers,
                    token,
                    anchor_entity,
                )
            }
        }
        impl<T: SymbolResolve + ?Sized> SymbolResolve for Arc<T> {
            fn resolve_session_entity(
                &self,
                token: &str,
            ) -> Result<EntityBinding, SymbolResolveError> {
                SymbolResolve::resolve_session_entity(self.as_ref(), token)
            }
            fn resolve_session_slot(
                &self,
                token: &str,
            ) -> Result<SlotBinding, SymbolResolveError> {
                SymbolResolve::resolve_session_slot(self.as_ref(), token)
            }
            fn resolve_session_method(
                &self,
                token: &str,
            ) -> Result<MethodBinding, SymbolResolveError> {
                SymbolResolve::resolve_session_method(self.as_ref(), token)
            }
            fn resolve_session_method_for_invoke(
                &self,
                token: &str,
                anchor_entity: &str,
            ) -> Result<MethodBinding, SymbolResolveError> {
                SymbolResolve::resolve_session_method_for_invoke(self.as_ref(), token, anchor_entity)
            }
            fn resolve_session_relation(
                &self,
                token: &str,
            ) -> Result<RelationBinding, SymbolResolveError> {
                SymbolResolve::resolve_session_relation(self.as_ref(), token)
            }
            fn resolve_entity_field(
                &self,
                catalog: CatalogScope<'_>,
                entity: &str,
                ent: &EntityDef,
                token: &str,
            ) -> Result<String, SymbolResolveError> {
                SymbolResolve::resolve_entity_field(self.as_ref(), catalog, entity, ent, token)
            }
            fn resolve_compound_key(
                &self,
                catalog: CatalogScope<'_>,
                entity: &str,
                key_vars: &[EntityFieldName],
                raw_key: &str,
            ) -> Result<String, SymbolResolveError> {
                SymbolResolve::resolve_compound_key(self.as_ref(), catalog, entity, key_vars, raw_key)
            }
            fn resolve_query_filter_field(
                &self,
                catalog: CatalogScope<'_>,
                entity: &str,
                ent: &EntityDef,
                cgs: &CGS,
                token: &str,
            ) -> Result<String, SymbolResolveError> {
                SymbolResolve::resolve_query_filter_field(
                    self.as_ref(),
                    catalog,
                    entity,
                    ent,
                    cgs,
                    token,
                )
            }
            fn resolve_cap_param(
                &self,
                catalog: CatalogScope<'_>,
                domain: &str,
                capability: &str,
                token: &str,
                invoke_cap: &CapabilitySchema,
            ) -> Result<String, SymbolResolveError> {
                SymbolResolve::resolve_cap_param(
                    self.as_ref(),
                    catalog,
                    domain,
                    capability,
                    token,
                    invoke_cap,
                )
            }
            fn resolve_opaque_session_method_capability<'a>(
                &self,
                layers: &[CgsLayer<'a>],
                token: &str,
                anchor_entity: &str,
            ) -> Result<&'a CapabilitySchema, SymbolResolveError> {
                SymbolResolve::resolve_opaque_session_method_capability(
                    self.as_ref(),
                    layers,
                    token,
                    anchor_entity,
                )
            }
        }
    };
}

macro_rules! delegate_symbol_render_deref {
    () => {
        impl<T: SymbolRender + ?Sized> SymbolRender for &T {
            fn resolve_binding_field_segment(&self, token: &str) -> String {
                SymbolRender::resolve_binding_field_segment(*self, token)
            }
            fn resolve_session_entity_symbol(&self, token: &str) -> Option<String> {
                SymbolRender::resolve_session_entity_symbol(*self, token)
            }
            fn resolve_method_symbol_token(&self, label: &str) -> Option<&str> {
                SymbolRender::resolve_method_symbol_token(*self, label)
            }
            fn resolve_method_symbol_triple(&self, label: &str) -> Option<(&str, &str, &str)> {
                SymbolRender::resolve_method_symbol_triple(*self, label)
            }
            fn entity_sym_for(&self, catalog_entry_id: &str, canonical: &str) -> String {
                SymbolRender::entity_sym_for(*self, catalog_entry_id, canonical)
            }
            fn method_sym_for(
                &self,
                catalog_entry_id: &str,
                domain: &str,
                capability: &str,
            ) -> String {
                SymbolRender::method_sym_for(*self, catalog_entry_id, domain, capability)
            }
            fn ident_sym_entity_field_for(
                &self,
                catalog_entry_id: &str,
                entity: &str,
                field: &str,
            ) -> String {
                SymbolRender::ident_sym_entity_field_for(*self, catalog_entry_id, entity, field)
            }
            fn ident_sym_cap_param_for(
                &self,
                catalog_entry_id: &str,
                domain: &str,
                capability: &str,
                param: &str,
            ) -> String {
                SymbolRender::ident_sym_cap_param_for(
                    *self,
                    catalog_entry_id,
                    domain,
                    capability,
                    param,
                )
            }
            fn cap_param_syms_hint(
                &self,
                catalog_entry_id: &str,
                domain: &str,
                capability: &str,
            ) -> String {
                SymbolRender::cap_param_syms_hint(*self, catalog_entry_id, domain, capability)
            }
            fn ident_sym_relation_for(
                &self,
                catalog_entry_id: &str,
                source_entity: &str,
                relation: &str,
            ) -> String {
                SymbolRender::ident_sym_relation_for(
                    *self,
                    catalog_entry_id,
                    source_entity,
                    relation,
                )
            }
            fn entity_stamps_for_wire(&self, wire: &str) -> Vec<(String, String)> {
                SymbolRender::entity_stamps_for_wire(*self, wire)
            }
            fn exposed_entity_symbol_rows(&self) -> Vec<ExposedEntitySymbolRow> {
                SymbolRender::exposed_entity_symbol_rows(*self)
            }
            fn entry_id_for_entity_symbol(&self, sym: &str) -> Option<String> {
                SymbolRender::entry_id_for_entity_symbol(*self, sym)
            }
        }
        impl<T: SymbolRender + ?Sized> SymbolRender for Arc<T> {
            fn resolve_binding_field_segment(&self, token: &str) -> String {
                SymbolRender::resolve_binding_field_segment(self.as_ref(), token)
            }
            fn resolve_session_entity_symbol(&self, token: &str) -> Option<String> {
                SymbolRender::resolve_session_entity_symbol(self.as_ref(), token)
            }
            fn resolve_method_symbol_token(&self, label: &str) -> Option<&str> {
                SymbolRender::resolve_method_symbol_token(self.as_ref(), label)
            }
            fn resolve_method_symbol_triple(&self, label: &str) -> Option<(&str, &str, &str)> {
                SymbolRender::resolve_method_symbol_triple(self.as_ref(), label)
            }
            fn entity_sym_for(&self, catalog_entry_id: &str, canonical: &str) -> String {
                SymbolRender::entity_sym_for(self.as_ref(), catalog_entry_id, canonical)
            }
            fn method_sym_for(
                &self,
                catalog_entry_id: &str,
                domain: &str,
                capability: &str,
            ) -> String {
                SymbolRender::method_sym_for(self.as_ref(), catalog_entry_id, domain, capability)
            }
            fn ident_sym_entity_field_for(
                &self,
                catalog_entry_id: &str,
                entity: &str,
                field: &str,
            ) -> String {
                SymbolRender::ident_sym_entity_field_for(self.as_ref(), catalog_entry_id, entity, field)
            }
            fn ident_sym_cap_param_for(
                &self,
                catalog_entry_id: &str,
                domain: &str,
                capability: &str,
                param: &str,
            ) -> String {
                SymbolRender::ident_sym_cap_param_for(
                    self.as_ref(),
                    catalog_entry_id,
                    domain,
                    capability,
                    param,
                )
            }
            fn cap_param_syms_hint(
                &self,
                catalog_entry_id: &str,
                domain: &str,
                capability: &str,
            ) -> String {
                SymbolRender::cap_param_syms_hint(self.as_ref(), catalog_entry_id, domain, capability)
            }
            fn ident_sym_relation_for(
                &self,
                catalog_entry_id: &str,
                source_entity: &str,
                relation: &str,
            ) -> String {
                SymbolRender::ident_sym_relation_for(
                    self.as_ref(),
                    catalog_entry_id,
                    source_entity,
                    relation,
                )
            }
            fn entity_stamps_for_wire(&self, wire: &str) -> Vec<(String, String)> {
                SymbolRender::entity_stamps_for_wire(self.as_ref(), wire)
            }
            fn exposed_entity_symbol_rows(&self) -> Vec<ExposedEntitySymbolRow> {
                SymbolRender::exposed_entity_symbol_rows(self.as_ref())
            }
            fn entry_id_for_entity_symbol(&self, sym: &str) -> Option<String> {
                SymbolRender::entry_id_for_entity_symbol(self.as_ref(), sym)
            }
        }
    };
}

delegate_symbol_resolve_deref!();
delegate_symbol_render_deref!();

/// Append-only symbol assignment during exposure waves — held only by the host commit path.
pub trait SymbolAllocate {
    fn snapshot(&self) -> Arc<dyn SymbolSession>;
}

impl SymbolAllocate for TeachingExposureSession {
    fn snapshot(&self) -> Arc<dyn SymbolSession> {
        self.symbol_map_arc()
    }
}
