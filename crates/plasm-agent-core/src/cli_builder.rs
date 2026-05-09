use clap::{Arg, Command};
use plasm_compile::{
    pagination_config_for_capability, parse_capability_template, path_var_names_from_request,
    template_var_names, CapabilityTemplate, CmlRequest, PaginationConfig, PaginationLocation,
    PaginationParam,
};
use std::collections::HashSet;

use plasm_core::{
    capability_method_label_kebab, CapabilityKind, CapabilityParamName, CapabilitySchema,
    EntityDef, FieldType, PromptRenderMode, RelationMaterialization, CGS,
};

use crate::invoke_args::build_invoke_args;
use crate::query_args::build_query_param_args;
use crate::subcommand_util::{
    field_subcommand_kebab, leak, path_param_arg_id, path_param_long_flag, pluralize_entity,
    relation_subcommand_kebab,
};

/// Built-in `--limit` / `--all` plus per-param flags derived from the composable pagination config.
fn append_pagination_args(mut cmd: Command, pconf: &PaginationConfig) -> Command {
    cmd = cmd.arg(
        Arg::new("pagination_limit")
            .long("limit")
            .help("Maximum entities to return (may issue multiple HTTP requests)")
            .value_parser(clap::value_parser!(usize)),
    );
    cmd = cmd.arg(
        Arg::new("pagination_all")
            .long("all")
            .action(clap::ArgAction::SetTrue)
            .help("Fetch all pages (runtime safety cap applies)"),
    );

    if pconf.location == PaginationLocation::BlockRange {
        cmd = cmd.arg(
            Arg::new("pagination_from_block")
                .long("from-block")
                .help("Starting EVM block number for block-range pagination")
                .value_parser(clap::value_parser!(u64)),
        );
        cmd = cmd.arg(
            Arg::new("pagination_to_block")
                .long("to-block")
                .help("Ending EVM block number for block-range pagination")
                .value_parser(clap::value_parser!(u64)),
        );
        return cmd;
    }

    // Expose starting-position flags for counter and from_response params.
    // At most one `--offset` (first counter whose name contains "offset") and one `--page`
    // (first other counter), so clap never sees duplicate arg ids.
    let mut added_offset_flag = false;
    let mut added_page_flag = false;
    let mut has_from_response = false;
    for (name, param) in &pconf.params {
        match param {
            PaginationParam::Counter { .. } => {
                let name_lower = name.to_lowercase();
                if name_lower.contains("offset") {
                    if !added_offset_flag {
                        added_offset_flag = true;
                        cmd = cmd.arg(
                            Arg::new("pagination_offset")
                                .long("offset")
                                .help(format!("Starting `{}` query parameter", name))
                                .value_parser(clap::value_parser!(i64)),
                        );
                    }
                } else if !added_page_flag {
                    added_page_flag = true;
                    cmd = cmd.arg(
                        Arg::new("pagination_page")
                            .long("page")
                            .help(format!("Starting `{}` query parameter", name))
                            .value_parser(clap::value_parser!(i64)),
                    );
                }
            }
            PaginationParam::FromResponse { .. } => {
                if !has_from_response {
                    has_from_response = true;
                    cmd = cmd.arg(
                        Arg::new("pagination_cursor")
                            .long("cursor")
                            .help(format!("Starting cursor token for `{}` param", name)),
                    );
                }
            }
            PaginationParam::Fixed { .. } => {}
        }
    }

    cmd
}

/// Append `--{kebab(var)}` for each path segment var except the last (filled by positional `id`).
fn append_multi_path_args(mut cmd: Command, cml: &CmlRequest) -> Command {
    let names = path_var_names_from_request(cml);
    if names.len() <= 1 {
        return cmd;
    }
    for var_name in names.iter().take(names.len() - 1) {
        let long: &'static str = leak(path_param_long_flag(var_name));
        cmd = cmd.arg(
            Arg::new(path_param_arg_id(var_name))
                .long(long)
                .required(true)
                .help(format!(
                    "Path parameter `{}` (URL segment before the resource id)",
                    var_name
                )),
        );
    }
    cmd
}

/// Required `--{key}` for each `key_vars` entry not already bound as an HTTP path segment.
fn append_compound_key_vars_not_on_path(
    mut cmd: Command,
    entity: &EntityDef,
    cml: Option<&CmlRequest>,
) -> Command {
    if entity.key_vars.len() <= 1 {
        return cmd;
    }
    let on_path: HashSet<String> = cml
        .map(|c| path_var_names_from_request(c).into_iter().collect())
        .unwrap_or_default();
    for kv in &entity.key_vars {
        if on_path.contains(kv.as_str()) {
            continue;
        }
        let long: &'static str = leak(path_param_long_flag(kv));
        cmd = cmd.arg(
            Arg::new(path_param_arg_id(kv))
                .long(long)
                .required(true)
                .help(format!(
                    "Compound key `{kv}` for {} (SCHEMA `key_vars`)",
                    entity.name
                )),
        );
    }
    cmd
}

fn append_get_template_var_args(mut cmd: Command, template: &CapabilityTemplate) -> Command {
    let http_path_vars = match template {
        CapabilityTemplate::Http(cml) | CapabilityTemplate::GraphQl(cml) => {
            path_var_names_from_request(cml)
        }
        CapabilityTemplate::View(_)
        | CapabilityTemplate::EvmCall(_)
        | CapabilityTemplate::EvmLogs(_) => Vec::new(),
    };

    for var_name in template_var_names(template) {
        if var_name == "id" || http_path_vars.contains(&var_name) {
            continue;
        }

        let long: &'static str = leak(path_param_long_flag(&var_name));
        cmd = cmd.arg(
            Arg::new(path_param_arg_id(&var_name))
                .long(long)
                .help(format!(
                    "Template variable `{}` for the GET capability",
                    var_name
                )),
        );
    }

    cmd
}

fn http_template_request(template: &serde_json::Value) -> Option<CmlRequest> {
    match parse_capability_template(template).ok()? {
        CapabilityTemplate::Http(cml) | CapabilityTemplate::GraphQl(cml) => Some(cml),
        CapabilityTemplate::View(_)
        | CapabilityTemplate::EvmCall(_)
        | CapabilityTemplate::EvmLogs(_) => None,
    }
}

/// Build the complete clap command tree from a CGS schema.
pub fn build_entity_commands(cgs: &CGS) -> Vec<Command> {
    cgs.entities
        .iter()
        .filter(|(_, entity)| !entity.abstract_entity)
        .map(|(name, entity)| build_entity_command(name, entity, cgs))
        .collect()
}

/// Entity command structure:
///
///   account query [--filters]           collection query (no ID)
///   account <ID>                        get by ID (implicit)
///   account <ID> contacts [--filters]   relation navigation
///   account <ID> update [--fields]      invoke capability
///
/// This reads as a natural graph path: entity -> node -> edge -> filters
///
/// Query filter flags are generated from the capability's `parameters:` only —
/// never from entity fields. No `parameters:` = no filter flags (correct for
/// pagination-only index endpoints such as PokéAPI resource lists).
fn build_entity_command(name: &str, entity: &EntityDef, cgs: &CGS) -> Command {
    let mut cmd = Command::new(leak(name.to_lowercase()))
        .about(format!("Operations on {} resources", name))
        .subcommand_required(false)
        .arg_required_else_help(true);

    // Primary query subcommand: the unscoped query capability (if any) gets "query" verb.
    if let Some(query_cap) = cgs.primary_query_capability(name) {
        let mut query_cmd = Command::new("query").about(format!("Query {} resources", name));
        for arg in build_query_param_args(query_cap, cgs) {
            query_cmd = query_cmd.arg(arg);
        }
        if let Some(ref pconf) = pagination_config_for_capability(query_cap) {
            query_cmd = append_pagination_args(query_cmd, pconf);
        }
        if cgs.find_capability(name, CapabilityKind::Get).is_some() {
            query_cmd = query_cmd.arg(
                Arg::new("query_summary")
                    .long("summary")
                    .action(clap::ArgAction::SetTrue)
                    .help(
                        "Return list-shaped rows only (skip automatic GET detail for each result)",
                    ),
            );
        }
        cmd = cmd.subcommand(query_cmd);
    }

    // Primary search subcommand: the unscoped search capability (if any) gets "search" verb.
    if let Some(search_cap) = cgs.primary_search_capability(name) {
        let mut search_cmd = Command::new("search").about(format!("Search {} by relevance", name));
        for arg in build_query_param_args(search_cap, cgs) {
            search_cmd = search_cmd.arg(arg);
        }
        if let Some(ref pconf) = pagination_config_for_capability(search_cap) {
            search_cmd = append_pagination_args(search_cmd, pconf);
        }
        cmd = cmd.subcommand(search_cmd);
    }

    // Named query/search subcommands: capabilities that aren't the primary query/search
    // get their own verb (e.g. "class-spells", "find-by-tags"). Includes scoped sub-resource
    // queries (role: scope required params) and secondary filter endpoints.
    for cap in cgs.named_query_capabilities(name) {
        let sub_kebab = capability_method_label_kebab(cap);
        let kind_label = match cap.kind {
            CapabilityKind::Search => "search",
            _ => "query",
        };
        let mut scoped_cmd =
            Command::new(leak(sub_kebab)).about(format!("{} (scoped {})", cap.name, kind_label));
        for arg in build_query_param_args(cap, cgs) {
            scoped_cmd = scoped_cmd.arg(arg);
        }
        if let Some(ref pconf) = pagination_config_for_capability(cap) {
            scoped_cmd = append_pagination_args(scoped_cmd, pconf);
        }
        cmd = cmd.subcommand(scoped_cmd);
    }

    // Positional ID -- when provided, enables node-level operations
    let id_help = if entity.key_vars.len() > 1 {
        format!(
            "Last URL path segment for {} (compound key {:?}; earlier parts use -- flags). Or follow with a subcommand.",
            name, entity.key_vars
        )
    } else {
        format!("{name} ID — get by ID, or follow with a subcommand")
    };
    cmd = cmd.arg(Arg::new("id").help(id_help));

    if let Some(get_cap) = cgs.find_capability(name, CapabilityKind::Get) {
        if let Ok(template) = parse_capability_template(&get_cap.mapping.template) {
            let http_cml = match &template {
                CapabilityTemplate::Http(cml) | CapabilityTemplate::GraphQl(cml) => Some(cml),
                _ => None,
            };
            if let Some(cml) = http_cml {
                cmd = append_multi_path_args(cmd, cml);
            }
            cmd = append_compound_key_vars_not_on_path(cmd, entity, http_cml);
            cmd = append_get_template_var_args(cmd, &template);
        }
    }

    // Node-level subcommands (require ID to be present):
    // These appear as sub-subcommands after the positional ID.

    // Relation navigation subcommands
    for (rel_name, rel_schema) in &entity.relations {
        let mut rel_cmd = Command::new(leak(relation_subcommand_kebab(rel_name))).about(format!(
            "{} -> {} ({})",
            name,
            rel_schema.target_resource,
            match rel_schema.cardinality {
                plasm_core::Cardinality::One => "one",
                plasm_core::Cardinality::Many => "many",
            },
        ));

        // Filter flags from the TARGET entity's query capability parameters.
        // For scoped relations, pick the capability that owns the scope param(s) and hide
        // those args (filled from the source entity when chaining).
        let (qcap, skip_params): (Option<&CapabilitySchema>, Vec<CapabilityParamName>) =
            match rel_schema.materialize.as_ref() {
                Some(RelationMaterialization::QueryScoped { capability, param }) => {
                    (cgs.get_capability(capability.as_str()), vec![param.clone()])
                }
                Some(RelationMaterialization::QueryScopedBindings {
                    capability,
                    bindings,
                }) => {
                    let keys: Vec<CapabilityParamName> = bindings.keys().cloned().collect();
                    let cap = cgs.get_capability(capability.as_str());
                    (cap, keys)
                }
                _ => (
                    cgs.find_capability(rel_schema.target_resource.as_str(), CapabilityKind::Query),
                    vec![],
                ),
            };
        if let Some(qcap) = qcap {
            for arg in build_query_param_args(qcap, cgs) {
                if skip_params.iter().any(|p| arg.get_id() == p.as_str()) {
                    continue;
                }
                rel_cmd = rel_cmd.arg(arg);
            }
            if let Some(ref pconf) = pagination_config_for_capability(qcap) {
                rel_cmd = append_pagination_args(rel_cmd, pconf);
            }
        }
        if cgs
            .find_capability(rel_schema.target_resource.as_str(), CapabilityKind::Get)
            .is_some()
        {
            rel_cmd = rel_cmd.arg(
                Arg::new("query_summary")
                    .long("summary")
                    .action(clap::ArgAction::SetTrue)
                    .help(
                        "Return list-shaped rows only (skip automatic GET detail for each result)",
                    ),
            );
        }

        cmd = cmd.subcommand(rel_cmd);
    }

    // Reverse-traversal subcommands: auto-derived from EntityRef back-index.
    // e.g. if Order.petId: EntityRef(Pet) and order_query has param petId: EntityRef(Pet),
    // then Pet gains subcommand `orders` → query(Order, petId=<this pet's id>).
    {
        let reverse_caps = cgs.find_reverse_traversal_caps(name);
        let relation_names: std::collections::HashSet<String> = entity
            .relations
            .keys()
            .map(|k| relation_subcommand_kebab(k))
            .collect();

        for (cap, param_name) in &reverse_caps {
            let sub_label = relation_subcommand_kebab(&pluralize_entity(cap.domain.as_str()));

            // Skip if a declared relation with this name already exists
            if relation_names.contains(&sub_label) {
                continue;
            }

            let mut rev_cmd = Command::new(leak(sub_label.clone())).about(format!(
                "{} ← {} (reverse via {}.{})",
                name, cap.domain, cap.domain, param_name,
            ));

            if let Some(qcap) = cgs.find_capability(cap.domain.as_str(), CapabilityKind::Query) {
                for arg in build_query_param_args(qcap, cgs) {
                    rev_cmd = rev_cmd.arg(arg);
                }
            }
            if let Some(qcap) = cgs.find_capability(cap.domain.as_str(), CapabilityKind::Query) {
                if let Some(ref pconf) = pagination_config_for_capability(qcap) {
                    rev_cmd = append_pagination_args(rev_cmd, pconf);
                }
            }
            if cgs
                .find_capability(cap.domain.as_str(), CapabilityKind::Get)
                .is_some()
            {
                rev_cmd = rev_cmd.arg(
                    Arg::new("query_summary")
                        .long("summary")
                        .action(clap::ArgAction::SetTrue)
                        .help("Return list-shaped rows only (skip automatic GET detail for each result)"),
                );
            }

            cmd = cmd.subcommand(rev_cmd);
        }
    }

    // EntityRef field navigation subcommands (FK auto-resolve)
    for (field_name, field_schema) in &entity.fields {
        let Ok(nv) = cgs.named_value_for_slot(field_schema) else {
            continue;
        };
        if let FieldType::EntityRef { ref target } = nv.field_type {
            let kebab: &'static str = leak(field_subcommand_kebab(field_name));
            if entity.relations.contains_key(field_name.as_str()) {
                continue;
            }
            let has_get = cgs
                .find_capability(target.as_str(), CapabilityKind::Get)
                .is_some();
            if !has_get {
                continue;
            }
            let ref_cmd = Command::new(kebab).about(format!(
                "{}.{} → {} (EntityRef auto-resolve)",
                name, field_name, target,
            ));
            cmd = cmd.subcommand(ref_cmd);
        }
    }

    // Mutation and action capability subcommands
    for cap in cgs.capabilities.values() {
        if cap.domain.as_str() != name {
            continue;
        }

        let sub_kebab = capability_method_label_kebab(cap);

        match cap.kind {
            // Query and Search: primary caps → "query"/"search" verb; scoped caps → named
            // subcommand. Both are generated above — skip here.
            CapabilityKind::Query | CapabilityKind::Search => {}
            CapabilityKind::Get => {}

            // Create: collection-level (no ID), with typed input fields
            CapabilityKind::Create => {
                let mut create_cmd =
                    Command::new(leak(sub_kebab.clone())).about(format!("Create a new {name}"));
                for arg in build_invoke_args(cap, cgs) {
                    create_cmd = create_cmd.arg(arg);
                }
                // Path vars for create come from the input object (runtime merges into CML env).
                cmd = cmd.subcommand(create_cmd);
            }

            // Delete: needs ID (node-level), no input fields
            CapabilityKind::Delete => {
                let mut del =
                    Command::new(leak(sub_kebab.clone())).about(format!("Delete a {name}"));
                if let Some(cml) = http_template_request(&cap.mapping.template) {
                    del = append_multi_path_args(del, &cml);
                }
                cmd = cmd.subcommand(del);
            }

            // Update/Action: needs ID (node-level), with typed input fields
            CapabilityKind::Update | CapabilityKind::Action => {
                let mut action_cmd =
                    Command::new(leak(sub_kebab.clone())).about(cap.name.to_string());
                for arg in build_invoke_args(cap, cgs) {
                    action_cmd = action_cmd.arg(arg);
                }
                if let Some(cml) = http_template_request(&cap.mapping.template) {
                    action_cmd = append_multi_path_args(action_cmd, &cml);
                }
                cmd = cmd.subcommand(action_cmd);
            }
        }
    }

    cmd
}

/// Which top-level binary / UX surface is being built (separate `clap` roots).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentCliSurface {
    /// SaaS core: HTTP + MCP (`plasm-mcp`).
    McpServer,
    /// Schema-driven subcommands (`plasm-cgs`).
    CgsClient,
    /// Interactive REPL (`plasm-repl`).
    Repl,
}

/// Build the top-level application command for a given surface.
pub fn build_app(cgs: &CGS, surface: AgentCliSurface) -> Command {
    let (bin_name, about) = match surface {
        AgentCliSurface::McpServer => (
            "plasm-mcp",
            "Plasm SaaS core — HTTP discovery / execute + MCP Streamable HTTP",
        ),
        AgentCliSurface::CgsClient => (
            "plasm-cgs",
            "Schema-driven CLI — generated subcommands from the CGS",
        ),
        AgentCliSurface::Repl => (
            "plasm-repl",
            "Interactive path-expression REPL against a live backend",
        ),
    };

    let mut app = Command::new(bin_name).about(about);

    app = app
        .arg(
            Arg::new("schema")
                .long("schema")
                .short('s')
                .help("Path to CGS schema directory or file"),
        )
        .arg(
            Arg::new("backend")
                .long("backend")
                .short('b')
                .default_value("http://localhost:1080")
                .help("Backend base URL (for bundled apis/github use https://api.github.com — github.com is the website host)"),
        )
        .arg(
            Arg::new("mode")
                .long("mode")
                .short('m')
                .default_value("live")
                .value_parser(["live", "replay", "hybrid"])
                .help("Execution mode"),
        )
        .arg(
            Arg::new("output")
                .long("output")
                .short('o')
                .default_value("json")
                .value_parser(["json", "table", "compact"])
                .help("Output format (REPL + one-shot CLI)"),
        )
        .arg(
            Arg::new("focus")
                .long("focus")
                .help("Schema prompt: focus entity (same as plasm-eval --focus)"),
        )
        .arg(
            Arg::new("symbol_tuning")
                .long("symbol-tuning")
                .default_value("tsv")
                .value_parser(PromptRenderMode::USER_FACING_VALUES)
                .help("Prompt render mode for schema/session instructions"),
        );

    match surface {
        AgentCliSurface::CgsClient => {
            app = app.subcommand_required(false);
            for entity_cmd in build_entity_commands(cgs) {
                app = app.subcommand(entity_cmd);
            }
            app
        }
        AgentCliSurface::McpServer => {
            app = app
                .arg(
                    Arg::new("plugin_dir")
                        .long("plugin-dir")
                        .value_name("DIR")
                        .help("Load catalogs from self-describing plugin cdylibs in this directory (ABI v4); use with --http/--mcp if omitting --schema"),
                )
                .arg(
                    Arg::new("http")
                        .long("http")
                        .action(clap::ArgAction::SetTrue)
                        .help("Listen for HTTP (discovery, execute, health)"),
                )
                .arg(
                    Arg::new("port")
                        .long("port")
                        .default_value("3000")
                        .value_parser(clap::value_parser!(u16))
                        .help("HTTP listen port"),
                )
                .arg(
                    Arg::new("mcp")
                        .long("mcp")
                        .action(clap::ArgAction::SetTrue)
                        .help("Listen for MCP Streamable HTTP (default path /mcp)"),
                )
                .arg(
                    Arg::new("mcp_port")
                        .long("mcp-port")
                        .value_parser(clap::value_parser!(u16))
                        .help("MCP port (default: --port+1 when both HTTP and MCP; else --port)"),
                )
                .arg(
                    Arg::new("compile_plugin")
                        .long("compile-plugin")
                        .help("Compile plugin cdylib (plasm-plugin-abi); execute sessions pin generation"),
                )
                .subcommand_required(false);
            app
        }
        AgentCliSurface::Repl => {
            app = app
                .arg(
                    Arg::new("plugin_dir")
                        .long("plugin-dir")
                        .value_name("DIR")
                        .help("Plugin cdylib directory (first entry's CGS when omitting --schema)"),
                )
                .subcommand_required(false);
            app
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::*;

    fn nv_string(cgs: &mut CGS, key: &str) {
        cgs.values.insert(
            key.into(),
            NamedValueSchema {
                description: String::new(),
                field_type: FieldType::String,
                value_format: None,
                allowed_values: None,
                string_semantics: Some(StringSemantics::Short),
                array_items: None,
            },
        );
    }
    fn nv_number(cgs: &mut CGS, key: &str) {
        cgs.values.insert(
            key.into(),
            NamedValueSchema {
                description: String::new(),
                field_type: FieldType::Number,
                value_format: None,
                allowed_values: None,
                string_semantics: None,
                array_items: None,
            },
        );
    }
    fn nv_select(cgs: &mut CGS, key: &str, allowed: Vec<String>) {
        cgs.values.insert(
            key.into(),
            NamedValueSchema {
                description: String::new(),
                field_type: FieldType::Select,
                value_format: None,
                allowed_values: Some(allowed),
                string_semantics: None,
                array_items: None,
            },
        );
    }
    fn nv_integer(cgs: &mut CGS, key: &str) {
        cgs.values.insert(
            key.into(),
            NamedValueSchema {
                description: String::new(),
                field_type: FieldType::Integer,
                value_format: None,
                allowed_values: None,
                string_semantics: None,
                array_items: None,
            },
        );
    }
    fn nv_entity_ref(cgs: &mut CGS, key: &str, target: EntityName) {
        cgs.values.insert(
            key.into(),
            NamedValueSchema {
                description: String::new(),
                field_type: FieldType::EntityRef { target },
                value_format: None,
                allowed_values: None,
                string_semantics: None,
                array_items: None,
            },
        );
    }

    /// Build a test CGS with:
    /// - Account entity (fields: id, name, revenue, region)
    /// - Contact entity (fields: id, name, role)
    /// - Account → Contact relation (contacts)
    /// - query_accounts: paginated, declares `region` as a query filter parameter
    /// - query_contacts: declares `role` as a query filter parameter (so relation filter works)
    fn test_cgs() -> CGS {
        let mut cgs = CGS::new();
        nv_string(&mut cgs, "cb_account_id");
        nv_string(&mut cgs, "cb_account_name");
        nv_number(&mut cgs, "cb_account_revenue");
        nv_select(
            &mut cgs,
            "cb_account_region",
            vec!["EMEA".into(), "APAC".into(), "AMER".into()],
        );
        nv_string(&mut cgs, "cb_contact_id");
        nv_string(&mut cgs, "cb_contact_name");
        nv_select(
            &mut cgs,
            "cb_contact_role",
            vec!["Manager".into(), "Employee".into()],
        );

        cgs.add_resource(ResourceSchema {
            name: "Account".into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: vec![
                FieldSchema {
                    name: "id".into(),
                    kind: FieldValueKind::Registry(
                        ValueDomainKey::new("cb_account_id").expect("key"),
                    ),
                    description: String::new(),
                    required: true,
                    agent_presentation: None,
                    mime_type_hint: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                },
                FieldSchema {
                    name: "name".into(),
                    kind: FieldValueKind::Registry(
                        ValueDomainKey::new("cb_account_name").expect("key"),
                    ),
                    description: String::new(),
                    required: true,
                    agent_presentation: None,
                    mime_type_hint: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                },
                FieldSchema {
                    name: "revenue".into(),
                    kind: FieldValueKind::Registry(
                        ValueDomainKey::new("cb_account_revenue").expect("key"),
                    ),
                    description: String::new(),
                    required: false,
                    agent_presentation: None,
                    mime_type_hint: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                },
                FieldSchema {
                    name: "region".into(),
                    kind: FieldValueKind::Registry(
                        ValueDomainKey::new("cb_account_region").expect("key"),
                    ),
                    description: String::new(),
                    required: false,
                    agent_presentation: None,
                    mime_type_hint: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                },
            ],
            relations: vec![RelationSchema {
                name: "contacts".into(),
                description: String::new(),
                target_resource: "Contact".into(),
                cardinality: Cardinality::Many,
                materialize: None,
                discovery: None,
            }],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: false,
            primary_read: None,
            discovery: None,
        })
        .unwrap();

        cgs.add_resource(ResourceSchema {
            name: "Contact".into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: vec![
                FieldSchema {
                    name: "id".into(),
                    kind: FieldValueKind::Registry(
                        ValueDomainKey::new("cb_contact_id").expect("key"),
                    ),
                    description: String::new(),
                    required: true,
                    agent_presentation: None,
                    mime_type_hint: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                },
                FieldSchema {
                    name: "name".into(),
                    kind: FieldValueKind::Registry(
                        ValueDomainKey::new("cb_contact_name").expect("key"),
                    ),
                    description: String::new(),
                    required: true,
                    agent_presentation: None,
                    mime_type_hint: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                },
                FieldSchema {
                    name: "role".into(),
                    kind: FieldValueKind::Registry(
                        ValueDomainKey::new("cb_contact_role").expect("key"),
                    ),
                    description: String::new(),
                    required: false,
                    agent_presentation: None,
                    mime_type_hint: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                },
            ],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: false,
            primary_read: None,
            discovery: None,
        })
        .unwrap();

        // query_accounts: paginated index with a declared `region` filter parameter
        cgs.add_capability(CapabilitySchema {
            name: "query_accounts".into(),
            description: String::new(),
            kind: CapabilityKind::Query,
            domain: "Account".into(),
            mapping: CapabilityMapping {
                template: serde_json::json!({
                    "method": "GET",
                    "path": [{"type": "literal", "value": "accounts"}],
                    "query": {
                        "type": "if",
                        "condition": {"type": "exists", "var": "region"},
                        "then_expr": {"type": "object", "fields": [["region", {"type": "var", "name": "region"}]]},
                        "else_expr": {"type": "object", "fields": []}
                    },
                    "pagination": {
                        "params": {
                            "offset": {"counter": 0},
                            "limit": {"fixed": 10}
                        }
                    }
                })
                .into(),
            },
            input_schema: Some(InputSchema {
                input_type: InputType::Object {
                    fields: vec![InputFieldSchema {
                        name: "region".into(),
                        wire: InputFieldWire::Registry(
                            ValueDomainKey::new("cb_account_region").expect("key"),
                        ),
                        required: false,
                        description: Some("Filter by region".into()),
                        default: None,
                        role: None,
                        wire_json_path: None,
                        wire_array_element_key: None,
                }],
                    additional_fields: false,
                },
                validation: InputValidation::default(),
                description: None,
                examples: vec![],
            }),
            output_schema: None,
            provides: vec![],
            scope_aggregate_key_policy: Default::default(),
            invoke_preflight: None,
            discovery: None,
        }).unwrap();

        // query_contacts: role filter parameter (enables contacts relation filter)
        cgs.add_capability(CapabilitySchema {
            name: "query_contacts".into(),
            description: String::new(),
            kind: CapabilityKind::Query,
            domain: "Contact".into(),
            mapping: CapabilityMapping {
                template: serde_json::json!({
                    "method": "GET",
                    "path": [{"type": "literal", "value": "contacts"}],
                    "query": {
                        "type": "if",
                        "condition": {"type": "exists", "var": "role"},
                        "then_expr": {"type": "object", "fields": [["role", {"type": "var", "name": "role"}]]},
                        "else_expr": {"type": "object", "fields": []}
                    }
                })
                .into(),
            },
            input_schema: Some(InputSchema {
                input_type: InputType::Object {
                    fields: vec![InputFieldSchema {
                        name: "role".into(),
                        wire: InputFieldWire::Registry(ValueDomainKey::new("cb_contact_role").expect("key")),
                        required: false,
                        description: Some("Filter by role".into()),
                        default: None,
                        role: None,
                        wire_json_path: None,
                        wire_array_element_key: None,
                }],
                    additional_fields: false,
                },
                validation: InputValidation::default(),
                description: None,
                examples: vec![],
            }),
            output_schema: None,
            provides: vec![],
            scope_aggregate_key_policy: Default::default(),
            invoke_preflight: None,
            discovery: None,
        }).unwrap();

        cgs
    }

    #[test]
    fn builds_entity_subcommands() {
        let cgs = test_cgs();
        let app = build_app(&cgs, AgentCliSurface::CgsClient);
        let subs: Vec<_> = app
            .get_subcommands()
            .map(|c| c.get_name().to_string())
            .collect();
        assert!(subs.contains(&"account".to_string()));
        assert!(subs.contains(&"contact".to_string()));
    }

    #[test]
    fn account_has_query_and_relations() {
        let cgs = test_cgs();
        let app = build_app(&cgs, AgentCliSurface::CgsClient);
        let account = app.find_subcommand("account").unwrap();
        let subs: Vec<_> = account
            .get_subcommands()
            .map(|c| c.get_name().to_string())
            .collect();
        assert!(subs.contains(&"query".to_string()));
        assert!(subs.contains(&"contacts".to_string()));
    }

    #[test]
    fn query_has_typed_flags() {
        let cgs = test_cgs();
        let app = build_app(&cgs, AgentCliSurface::CgsClient);
        let account = app.find_subcommand("account").unwrap();
        let query = account.find_subcommand("query").unwrap();
        let arg_names: Vec<_> = query
            .get_arguments()
            .map(|a| a.get_id().as_str().to_string())
            .collect();

        // Declared capability parameter → flag present
        assert!(
            arg_names.contains(&"region".to_string()),
            "region param should exist: {:?}",
            arg_names
        );

        // Entity fields that are NOT capability parameters must NOT generate flags
        assert!(
            !arg_names.contains(&"name".to_string()),
            "name is an entity field, not a query param"
        );
        assert!(
            !arg_names.contains(&"revenue".to_string()),
            "revenue is an entity field, not a query param"
        );
        assert!(
            !arg_names.contains(&"revenue-gt".to_string()),
            "operator suffixes must not appear"
        );

        // Pagination flags from CML block
        assert!(arg_names.contains(&"pagination_limit".to_string()));
        assert!(arg_names.contains(&"pagination_all".to_string()));
        assert!(arg_names.contains(&"pagination_offset".to_string()));
        assert!(!arg_names.contains(&"pagination_page".to_string()));
        assert!(!arg_names.contains(&"pagination_cursor".to_string()));
    }

    #[test]
    fn contacts_has_target_entity_filters() {
        // Relation filter flags come from the target's query capability parameters,
        // not from entity fields.
        let cgs = test_cgs();
        let app = build_app(&cgs, AgentCliSurface::CgsClient);
        let account = app.find_subcommand("account").unwrap();
        let contacts = account.find_subcommand("contacts").unwrap();
        let arg_names: Vec<_> = contacts
            .get_arguments()
            .map(|a| a.get_id().as_str().to_string())
            .collect();
        assert!(
            arg_names.contains(&"role".to_string()),
            "role is a query param on Contact: {:?}",
            arg_names
        );

        // Entity-field-derived flags must not appear
        assert!(
            !arg_names.contains(&"name".to_string()),
            "name is an entity field, not a declared param"
        );
    }

    #[test]
    fn parses_query_end_to_end() {
        let cgs = test_cgs();
        let app = build_app(&cgs, AgentCliSurface::CgsClient);
        let matches = app
            .try_get_matches_from(["plasm-agent", "account", "query", "--region", "EMEA"])
            .unwrap();

        let (entity_name, entity_matches) = matches.subcommand().unwrap();
        assert_eq!(entity_name, "account");
        let (sub_name, sub_matches) = entity_matches.subcommand().unwrap();
        assert_eq!(sub_name, "query");

        let query_cap = cgs
            .find_capability("Account", CapabilityKind::Query)
            .unwrap();
        let pred =
            crate::query_args::args_to_query_predicate(sub_matches, query_cap, &cgs).unwrap();
        if let Predicate::Comparison { field, value, .. } = &pred {
            assert_eq!(field, "region");
            assert_eq!(value.to_value(), plasm_core::Value::String("EMEA".into()));
        } else {
            panic!("expected single Comparison, got {:?}", pred);
        }
    }

    #[test]
    fn parses_id_alone_as_get() {
        let cgs = test_cgs();
        let app = build_app(&cgs, AgentCliSurface::CgsClient);
        let matches = app
            .try_get_matches_from(["plasm-agent", "account", "acc-1"])
            .unwrap();

        let (_, entity_matches) = matches.subcommand().unwrap();
        // No subcommand means ID-only = get
        assert!(entity_matches.subcommand().is_none());
        assert_eq!(entity_matches.get_one::<String>("id").unwrap(), "acc-1");
    }

    #[test]
    fn parses_id_then_relation() {
        let cgs = test_cgs();
        let app = build_app(&cgs, AgentCliSurface::CgsClient);
        let matches = app
            .try_get_matches_from([
                "plasm-agent",
                "account",
                "acc-1",
                "contacts",
                "--role",
                "Manager",
            ])
            .unwrap();

        let (_, entity_matches) = matches.subcommand().unwrap();
        assert_eq!(entity_matches.get_one::<String>("id").unwrap(), "acc-1");
        let (sub_name, sub_matches) = entity_matches.subcommand().unwrap();
        assert_eq!(sub_name, "contacts");
        assert_eq!(sub_matches.get_one::<String>("role").unwrap(), "Manager");
    }

    fn test_cgs_with_evm_get_var() -> CGS {
        let mut cgs = CGS::new();
        nv_string(&mut cgs, "cb_evm_account");
        nv_string(&mut cgs, "cb_evm_balance");

        cgs.add_resource(ResourceSchema {
            name: "Balance".into(),
            description: String::new(),
            id_field: "account".into(),
            id_format: None,
            id_from: None,
            fields: vec![
                FieldSchema {
                    name: "account".into(),
                    kind: FieldValueKind::Registry(
                        ValueDomainKey::new("cb_evm_account").expect("key"),
                    ),
                    description: String::new(),
                    required: true,
                    agent_presentation: None,
                    mime_type_hint: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                },
                FieldSchema {
                    name: "balance".into(),
                    kind: FieldValueKind::Registry(
                        ValueDomainKey::new("cb_evm_balance").expect("key"),
                    ),
                    description: String::new(),
                    required: true,
                    agent_presentation: None,
                    mime_type_hint: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                },
            ],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: false,
            primary_read: None,
            discovery: None,
        })
        .unwrap();

        cgs.add_capability(CapabilitySchema {
            name: "balance_get".into(),
            description: String::new(),
            kind: CapabilityKind::Get,
            domain: "Balance".into(),
            mapping: CapabilityMapping {
                template: serde_json::json!({
                    "transport": "evm_call",
                    "chain": 1,
                    "contract": { "type": "const", "value": "0x0000000000000000000000000000000000000001" },
                    "function": "function balanceOf(address owner) view returns (uint256)",
                    "args": [{ "type": "var", "name": "id" }],
                    "block": { "type": "var", "name": "block" }
                })
                .into(),
            },
            input_schema: None,
            output_schema: None,
            provides: vec![],
            scope_aggregate_key_policy: Default::default(),
            invoke_preflight: None,
            discovery: None,
        })
        .unwrap();

        cgs
    }

    fn test_cgs_with_entity_ref() -> CGS {
        let mut cgs = CGS::new();
        nv_integer(&mut cgs, "cb_order_id");
        nv_entity_ref(&mut cgs, "cb_order_pet_ref", "Pet".into());
        nv_integer(&mut cgs, "cb_pet_id");
        nv_string(&mut cgs, "cb_pet_name");

        cgs.add_resource(ResourceSchema {
            name: "Order".into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: vec![
                FieldSchema {
                    name: "id".into(),
                    kind: FieldValueKind::Registry(
                        ValueDomainKey::new("cb_order_id").expect("key"),
                    ),
                    description: String::new(),
                    required: true,
                    agent_presentation: None,
                    mime_type_hint: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                },
                FieldSchema {
                    name: "petId".into(),
                    kind: FieldValueKind::Registry(
                        ValueDomainKey::new("cb_order_pet_ref").expect("key"),
                    ),
                    description: String::new(),
                    required: true,
                    agent_presentation: None,
                    mime_type_hint: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                },
            ],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: false,
            primary_read: None,
            discovery: None,
        })
        .unwrap();

        cgs.add_resource(ResourceSchema {
            name: "Pet".into(),
            description: String::new(),
            id_field: "id".into(),
            id_format: None,
            id_from: None,
            fields: vec![
                FieldSchema {
                    name: "id".into(),
                    kind: FieldValueKind::Registry(ValueDomainKey::new("cb_pet_id").expect("key")),
                    description: String::new(),
                    required: true,
                    agent_presentation: None,
                    mime_type_hint: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                },
                FieldSchema {
                    name: "name".into(),
                    kind: FieldValueKind::Registry(
                        ValueDomainKey::new("cb_pet_name").expect("key"),
                    ),
                    description: String::new(),
                    required: true,
                    agent_presentation: None,
                    mime_type_hint: None,
                    attachment_media: None,
                    wire_path: None,
                    derive: None,
                },
            ],
            relations: vec![],
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: false,
            primary_read: None,
            discovery: None,
        })
        .unwrap();

        cgs.add_capability(CapabilitySchema {
            name: "order_get".into(),
            description: String::new(),
            kind: CapabilityKind::Get,
            domain: "Order".into(),
            mapping: CapabilityMapping {
                template: serde_json::json!({
                    "method": "GET",
                    "path": [{"type": "literal", "value": "store"}, {"type": "literal", "value": "order"}, {"type": "var", "name": "id"}],
                })
                .into(),
            },
            input_schema: None,
            output_schema: None,
            provides: vec![],
            scope_aggregate_key_policy: Default::default(),
            invoke_preflight: None,
            discovery: None,
        }).unwrap();

        cgs.add_capability(CapabilitySchema {
            name: "pet_get".into(),
            description: String::new(),
            kind: CapabilityKind::Get,
            domain: "Pet".into(),
            mapping: CapabilityMapping {
                template: serde_json::json!({
                    "method": "GET",
                    "path": [{"type": "literal", "value": "pet"}, {"type": "var", "name": "id"}],
                })
                .into(),
            },
            input_schema: None,
            output_schema: None,
            provides: vec![],
            scope_aggregate_key_policy: Default::default(),
            invoke_preflight: None,
            discovery: None,
        })
        .unwrap();

        cgs
    }

    #[test]
    fn entity_ref_generates_subcommand() {
        let cgs = test_cgs_with_entity_ref();
        let app = build_app(&cgs, AgentCliSurface::CgsClient);
        let order = app.find_subcommand("order").unwrap();
        let subs: Vec<_> = order
            .get_subcommands()
            .map(|c| c.get_name().to_string())
            .collect();
        assert!(
            subs.contains(&"pet-id".to_string()),
            "expected 'pet-id' subcommand for EntityRef field petId, got {:?}",
            subs
        );
    }

    fn test_cgs_with_reverse_traversal() -> CGS {
        let mut cgs = test_cgs_with_entity_ref();

        cgs.add_capability(CapabilitySchema {
            name: "order_query".into(),
            description: String::new(),
            kind: CapabilityKind::Query,
            domain: "Order".into(),
            mapping: CapabilityMapping {
                template: serde_json::json!({
                    "method": "GET",
                    "path": [{"type": "literal", "value": "store"}, {"type": "literal", "value": "order"}],
                })
                .into(),
            },
            input_schema: Some(InputSchema {
                input_type: InputType::Object {
                    fields: vec![InputFieldSchema {
                        name: "petId".into(),
                        wire: InputFieldWire::Registry(ValueDomainKey::new("cb_order_pet_ref").expect("key")),
                        required: false,
                        description: None,
                        default: None,
                        role: None,
                        wire_json_path: None,
                        wire_array_element_key: None,
                }],
                    additional_fields: true,
                },
                validation: InputValidation::default(),
                description: None,
                examples: vec![],
            }),
            output_schema: None,
            provides: vec![],
            scope_aggregate_key_policy: Default::default(),
            invoke_preflight: None,
            discovery: None,
        }).unwrap();

        cgs
    }

    #[test]
    fn reverse_traversal_generates_subcommand() {
        let cgs = test_cgs_with_reverse_traversal();
        let app = build_app(&cgs, AgentCliSurface::CgsClient);
        let pet = app.find_subcommand("pet").unwrap();
        let subs: Vec<_> = pet
            .get_subcommands()
            .map(|c| c.get_name().to_string())
            .collect();
        assert!(
            subs.contains(&"orders".to_string()),
            "expected 'orders' reverse-traversal subcommand on Pet, got {:?}",
            subs
        );
    }

    #[test]
    fn reverse_traversal_parses() {
        let cgs = test_cgs_with_reverse_traversal();
        let app = build_app(&cgs, AgentCliSurface::CgsClient);
        let matches = app
            .try_get_matches_from(["plasm-agent", "pet", "10", "orders"])
            .unwrap();

        let (entity_name, entity_matches) = matches.subcommand().unwrap();
        assert_eq!(entity_name, "pet");
        assert_eq!(entity_matches.get_one::<String>("id").unwrap(), "10");
        let (sub_name, _) = entity_matches.subcommand().unwrap();
        assert_eq!(sub_name, "orders");
    }

    #[test]
    fn parses_entity_ref_navigation() {
        let cgs = test_cgs_with_entity_ref();
        let app = build_app(&cgs, AgentCliSurface::CgsClient);
        let matches = app
            .try_get_matches_from(["plasm-agent", "order", "5", "pet-id"])
            .unwrap();

        let (entity_name, entity_matches) = matches.subcommand().unwrap();
        assert_eq!(entity_name, "order");
        assert_eq!(entity_matches.get_one::<String>("id").unwrap(), "5");
        let (sub_name, _) = entity_matches.subcommand().unwrap();
        assert_eq!(sub_name, "pet-id");
    }

    #[test]
    fn get_command_exposes_evm_template_vars() {
        let cgs = test_cgs_with_evm_get_var();
        let app = build_app(&cgs, AgentCliSurface::CgsClient);
        let balance = app.find_subcommand("balance").unwrap();
        let arg_names: Vec<_> = balance
            .get_arguments()
            .map(|a| a.get_id().as_str().to_string())
            .collect();

        assert!(
            arg_names.contains(&path_param_arg_id("block").to_string()),
            "{arg_names:?}"
        );

        let matches = app
            .try_get_matches_from(["plasm-agent", "balance", "0xabc", "--block", "latest"])
            .unwrap();
        let (_, entity_matches) = matches.subcommand().unwrap();
        assert_eq!(
            entity_matches
                .get_one::<String>(path_param_arg_id("block"))
                .map(String::as_str),
            Some("latest")
        );
    }
}
