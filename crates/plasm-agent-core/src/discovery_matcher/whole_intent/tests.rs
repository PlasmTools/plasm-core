use super::admission::{record_identity, QualifiedSubjects};
use super::*;

#[test]
fn scoped_record_returns_the_child_and_checks_both_key_domains() {
    let path = std::path::Path::new(crate::test_manifest_dir())
        .join("../../fixtures/schemas/plasm_prompt_matrix");
    let mut cgs = plasm_core::loader::load_schema_dir(&path).unwrap();
    let zone = cgs.entities.get_mut("Zone").unwrap();
    let mut id = zone.fields.shift_remove("id").unwrap();
    id.name = "zone_id".into();
    zone.id_field = id.name.clone();
    zone.fields.insert(id.name.clone(), id);
    cgs.entities
        .get_mut("Ruleset")
        .unwrap()
        .discovery
        .get_or_insert_with(Default::default)
        .names
        .push("rule set".into());
    let mut catalogs = BTreeMap::from([("matrix".into(), cgs)]);
    let intent = "Read rule set sample in the matrix zone west and return its name.";
    let expected = BTreeSet::from([CapabilityRef {
        catalog: "matrix".into(),
        capability: "ruleset_get".into(),
    }]);
    assert_eq!(
        References::record_selection(intent, &catalogs),
        Some((expected, true))
    );
    assert!(References::record_selection(
        "Read rule set sample in the matrix zone west and return its imaginary_field.",
        &catalogs
    )
    .is_none());
    assert!(matches!(
        References::record_selection("Read rule set sample in the matrix zone west.", &catalogs),
        Some((_, false))
    ));
    assert!(References::record_reads(
        "Read rule set #42 in the matrix zone west and return its name.",
        &catalogs
    )
    .is_none());
    let cgs = catalogs.get_mut("matrix").unwrap();
    let different = cgs.entities["Ruleset"].fields["description"].kind.clone();
    cgs.entities
        .get_mut("Ruleset")
        .unwrap()
        .fields
        .get_mut("zone_id")
        .unwrap()
        .kind = different;
    assert!(References::record_reads(intent, &catalogs).is_none());
}

#[test]
fn natural_collection_names_nominate_only_authorized_unquoted_queries() {
    let path = std::path::Path::new(crate::test_manifest_dir())
        .join("../../fixtures/schemas/plasm_language_matrix");
    let catalogs = BTreeMap::from([(
        "matrix".into(),
        plasm_core::loader::load_schema_dir(&path).unwrap(),
    )]);
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let request = "List query only requests and return their numbers.";
    assert_eq!(
        named_collection_queries(request, &catalogs, &allowed),
        BTreeSet::from([CapabilityRef {
            catalog: "matrix".into(),
            capability: "queryonly_request_received_query".into()
        }])
    );
    assert!(named_collection_queries(
        "Find text matching 'query only requests'",
        &catalogs,
        &allowed
    )
    .is_empty());
    assert!(named_collection_queries(
        request,
        &catalogs,
        &DiscoveryAuthorization::catalogs(BTreeSet::new())
    )
    .is_empty());
}

#[test]
#[ignore = "diagnostic replay of an explicitly supplied production receipt and catalog snapshot"]
fn production_packet_profile() {
    let receipt: Value = serde_json::from_slice(
        &std::fs::read(std::env::var("PLASM_CAPTURED_RECEIPT").unwrap()).unwrap(),
    )
    .unwrap();
    let recall: RetrievalReceipt = serde_json::from_value(receipt["retrieval"].clone()).unwrap();
    let allowed: DiscoveryAuthorization =
        serde_json::from_value(receipt["authorization"].clone()).unwrap();
    let root = std::path::PathBuf::from(std::env::var("PLASM_CAPTURED_CATALOG_DIR").unwrap());
    let paths: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|p| p.unwrap().path())
        .collect();
    let mut catalogs = BTreeMap::new();
    for name in &allowed.catalogs {
        let prefix = format!("{name}.");
        let path = paths.iter().find(|p| {
            p.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with(&prefix)
                && p.to_str().unwrap().ends_with(".cgs.json")
        });
        let Some(path) = path else {
            assert!(recall
                .candidates
                .iter()
                .all(|c| &c.reference.catalog != name));
            continue;
        };
        let cgs: CGS = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        catalogs.insert(name.clone(), cgs);
    }
    let deployments: DeploymentBindings =
        serde_json::from_slice(&std::fs::read(root.join("deployment-bindings.json")).unwrap())
            .unwrap();
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    for candidate in &recall.candidates {
        assert_eq!(
            serde_json::to_value(&index.candidate(&candidate.reference).unwrap().document).unwrap(),
            serde_json::to_value(&candidate.document).unwrap()
        );
    }
    let packet = WholeIntentPacket::new(
        DEFAULT_MODEL,
        receipt["intent"].as_str().unwrap(),
        &recall,
        &catalogs,
        &allowed,
        &index,
        &deployments,
        &BTreeMap::new(),
        &[],
    )
    .unwrap();
    println!(
        "CAPTURE_PROFILE recalled={} assessed={} deferred={} projection_limited={} bytes={}",
        recall.candidates.len(),
        packet.candidates.len(),
        packet.omitted.len(),
        packet
            .omitted
            .iter()
            .filter(|r| !packet.was_projected(r))
            .count(),
        packet.body.len()
    );
}
use crate::discovery_matcher::catalogue_search::CatalogueSearch;
use plasm_core::prerequisites::DeploymentBindings;

fn packet_with_priority(prioritize: bool) -> WholeIntentPacket {
    let path = std::path::Path::new(crate::test_manifest_dir())
        .join("../../fixtures/schemas/plasm_prompt_matrix");
    let mut cgs = plasm_core::loader::load_schema_dir(&path).unwrap();
    cgs.bind_registry_entry_id("matrix");
    let catalogs = BTreeMap::from([("matrix".to_owned(), cgs)]);
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".to_owned()]));
    let bindings = DeploymentBindings::default();
    let index = CatalogueSearch::new(&catalogs, &bindings, &allowed).unwrap();
    let references: BTreeSet<_> = index
        .authorized_capabilities(&allowed)
        .into_iter()
        .take(2)
        .collect();
    assert_eq!(references.len(), 2);
    let last = references.iter().next_back().unwrap().clone();
    let priorities = if prioritize {
        BTreeMap::from([(last, 3)])
    } else {
        BTreeMap::new()
    };
    let (recall, _) = index
        .evidence("g", &references, &catalogs, &bindings, &allowed)
        .unwrap();
    WholeIntentPacket::new(
        DEFAULT_MODEL,
        "Read a record; do not change it.",
        &recall,
        &catalogs,
        &allowed,
        &index,
        &bindings,
        &priorities,
        &[],
    )
    .unwrap()
}

fn packet() -> WholeIntentPacket {
    packet_with_priority(false)
}

#[test]
fn column_groups_preserve_heterogeneous_fields_and_explicit_nulls() {
    let original = json!({
        "first": {"name":"α", "required":false, "detail":null},
        "second": {"name":"β", "required":false},
        "third": {"name":"γ", "required":true, "detail":{"allowed":[1,2]}}
    });
    let groups = column_groups(original.as_object().unwrap()).unwrap();
    let mut restored = Map::new();
    for group in groups.as_array().unwrap() {
        let columns = group["columns"].as_array().unwrap();
        for (name, row) in group["rows"].as_object().unwrap() {
            let row = row.as_array().unwrap();
            assert_eq!(columns.len(), row.len());
            let object = columns
                .iter()
                .zip(row)
                .map(|(key, value)| (key.as_str().unwrap().to_owned(), value.clone()))
                .collect();
            assert!(restored
                .insert(name.clone(), Value::Object(object))
                .is_none());
        }
    }
    assert_eq!(Value::Object(restored), original);
}

#[test]
fn compact_bindings_preserve_directed_edges_and_metadata() {
    use super::super::witnesses::{Binding, Endpoint, Slot};
    use plasm_core::prerequisites::{ArgumentSource, InputLane, InputPath};
    let a = CapabilityRef {
        catalog: "a".into(),
        capability: "first".into(),
    };
    let b = CapabilityRef {
        catalog: "b".into(),
        capability: "second".into(),
    };
    let c = CapabilityRef {
        catalog: "c".into(),
        capability: "third".into(),
    };
    let hidden = CapabilityRef {
        catalog: "d".into(),
        capability: "hidden".into(),
    };
    let endpoint = |reference: &CapabilityRef, field: &str| Endpoint {
        capability: reference.clone(),
        slot: Slot::Field {
            field: field.into(),
        },
    };
    let edges = vec![
        Binding::Membership {
            from: endpoint(&a, "id"),
            to: endpoint(&b, "id"),
        },
        Binding::Membership {
            from: endpoint(&a, "id"),
            to: endpoint(&c, "key"),
        },
        Binding::Membership {
            from: endpoint(&b, "id"),
            to: endpoint(&a, "id"),
        },
        Binding::Membership {
            from: endpoint(&a, "id"),
            to: endpoint(&b, "id"),
        },
        Binding::Membership {
            from: endpoint(&a, "id"),
            to: endpoint(&hidden, "id"),
        },
        Binding::InputSource {
            from: endpoint(&a, "id"),
            to: endpoint(&b, "value"),
            collect: true,
        },
        Binding::MayCreateResource {
            from: endpoint(&b, "id"),
            to: endpoint(&c, "key"),
        },
        Binding::Prerequisite {
            from: endpoint(&c, "key"),
            to: endpoint(&a, "id"),
            arguments: BTreeMap::from([(
                "token".into(),
                ArgumentSource::Constant {
                    value: json!({"nested":[1,null,"value"]}),
                },
            )]),
            provider_inputs: BTreeMap::from([(
                "key".into(),
                InputPath {
                    lane: InputLane::Arguments,
                    path: vec!["nested".into(), "id".into()],
                },
            )]),
        },
    ];
    let ids = BTreeMap::from([(a, "c0".into()), (b, "c1".into()), (c, "c2".into())]);
    let mut expected = Vec::new();
    for edge in &edges {
        let endpoints = edge.endpoints();
        let (Some(from), Some(to)) = (
            ids.get(&endpoints[0].capability),
            ids.get(&endpoints[1].capability),
        ) else {
            continue;
        };
        let mut value = serde_json::to_value(edge).unwrap();
        value["from"]["capability"] = json!(from);
        value["to"]["capability"] = json!(to);
        expected.push(value.to_string());
    }
    let mut actual = Vec::new();
    let mut cache: Vec<Option<BindingProjection>> = (0..edges.len()).map(|_| None).collect();
    for mut edge in compact_bindings(&edges, &ids, &mut cache).unwrap() {
        if edge["kind"] == "membership" {
            let targets = edge["to"].take().as_array().unwrap().clone();
            for target in targets {
                edge["to"] = target;
                actual.push(edge.to_string());
            }
        } else {
            actual.push(edge.to_string());
        }
    }
    expected.sort();
    actual.sort();
    assert_eq!(actual, expected);
    let mut changed_ids = ids.clone();
    *changed_ids
        .get_mut(&CapabilityRef {
            catalog: "a".into(),
            capability: "first".into(),
        })
        .unwrap() = "c9".into();
    let changed =
        serde_json::to_string(&compact_bindings(&edges, &changed_ids, &mut cache).unwrap())
            .unwrap();
    assert!(changed.contains("\"c9\""));
    assert!(!changed.contains("\"c0\""));
}

#[test]
fn borrowed_packet_encoding_matches_value_encoding() {
    let values = json!([{"columns":["description"],"rows":{"v0":["text"]}}]);
    let records = json!({"catalog/Record":{"fields":[],"id_field":"id"}});
    let records = records.as_object().unwrap();
    let cards = vec![json!({"id":"c0","operation":{"kind":"get"}})];
    let bindings = vec![json!({"kind":"binding","source":{"catalog":"a","capability":"b"}})];
    let packet = EvidencePacket {
        bindings: json!(bindings),
        candidates: &cards,
        definitions: EvidenceDefinitions {
            records,
            values: values.clone(),
        },
        format: EVIDENCE_FORMAT,
        intent: "Read a record.",
    };
    let reference = json!({"intent":"Read a record.","format":EVIDENCE_FORMAT,
        "candidates":cards,"definitions":{"values":values,"records":records},"bindings":bindings});
    assert_eq!(
        serde_json::to_string(&packet).unwrap(),
        reference.to_string()
    );
}

#[test]
fn flattened_candidate_preserves_contract_and_scoped_references() {
    let reference = CapabilityRef {
        catalog: "vendor/with%escape".into(),
        capability: "read".into(),
    };
    let prefix = super::super::evidence::key(&reference.catalog, "");
    let cards = json!({"c0":{
        "capability":{"reference":reference,"entity":"Record","operation":{"kind":"get","receiver":null,"contract":"Read a record."}},
        "declaration":{"provides":["id"],"output":null}
    }});
    let refs = json!({"c0":{"values":[format!("{prefix}value%2Fid")],"records":[format!("{prefix}Record")]}});
    let flat = flat_candidate(&reference, "c0", &cards, &refs).unwrap();
    assert_eq!(flat["catalog"], reference.catalog);
    assert_eq!(flat["capability"], reference.capability);
    assert_eq!(flat["entity"], cards["c0"]["capability"]["entity"]);
    assert_eq!(flat["operation"], cards["c0"]["capability"]["operation"]);
    assert_eq!(flat["declaration"], cards["c0"]["declaration"]);
    for kind in ["values", "records"] {
        let rebuilt: Vec<_> = flat["references"][kind]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| format!("{prefix}{}", v.as_str().unwrap()))
            .collect();
        assert_eq!(json!(rebuilt), refs["c0"][kind]);
    }
    let foreign = json!({"c0":{"values":["other/value"],"records":[]}});
    assert!(flat_candidate(&reference, "c0", &cards, &foreign).is_err());
}

#[test]
fn batch_admission_matches_individual_greedy_admission() {
    let original = packet();
    let mut candidates = Vec::new();
    for (i, size) in [12_000, MAX_BODY_BYTES + 1_000, 28_000, 26_000, 8_000]
        .into_iter()
        .enumerate()
    {
        let mut candidate = original.candidates[0].clone();
        candidate.reference.capability = format!("candidate_{i}");
        candidates.push(PreparedCandidate {
            question: AccessQuestion::Read("Is this operation needed?"),
            candidate,
            card: json!({"id":"c0", "catalog":"matrix", "entity":"Record", "capability":format!("candidate_{i}"), "operation":{"contract":"Read records."}, "payload":"x".repeat(size)}),
            values: vec!["shared".into()],
            records: Vec::new(),
        });
    }
    let evidence = super::super::evidence::CatalogEvidence::default();
    let values = Map::from_iter([("shared".into(), json!({"description":"shared"}))]);
    let records = BTreeMap::new();
    let make_assembly = || PacketAssembly {
        scope: EvidenceScope::WholeIntent,
        byte_limit: MAX_BODY_BYTES,
        model: DEFAULT_MODEL,
        intent: "Read records.",
        evidence: &evidence,
        all_values: &values,
        all_records: &records,
        cards: Vec::new(),
        prompts: Vec::new(),
        candidates: Vec::new(),
        values: Map::new(),
        records: Map::new(),
        included: BTreeMap::new(),
        binding_cache: Vec::new(),
        body: None,
    };
    let mut batched = make_assembly();
    batched.admit(&candidates).unwrap();
    let mut greedy = make_assembly();
    for candidate in &candidates {
        greedy.admit(std::slice::from_ref(candidate)).unwrap();
    }
    assert_eq!(batched.body, greedy.body);
    assert_eq!(batched.included, greedy.included);
    assert!(batched
        .included
        .contains_key(&candidates[4].candidate.reference));
    assert!(!batched
        .included
        .contains_key(&candidates[1].candidate.reference));
}

#[test]
fn oversized_candidate_is_deferred_without_blocking_a_later_fit() {
    let path = std::path::Path::new(crate::test_manifest_dir())
        .join("../../fixtures/schemas/plasm_prompt_matrix");
    let mut cgs = plasm_core::loader::load_schema_dir(&path).unwrap();
    cgs.bind_registry_entry_id("matrix");
    let mut names: Vec<_> = cgs.capabilities.keys().cloned().collect();
    names.sort();
    names.truncate(3);
    assert_eq!(names.len(), 3);
    cgs.capabilities.get_mut(&names[1]).unwrap().description = "x".repeat(MAX_BODY_BYTES);
    let catalogs = BTreeMap::from([("matrix".to_owned(), cgs)]);
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".to_owned()]));
    let bindings = DeploymentBindings::default();
    let index = CatalogueSearch::new(&catalogs, &bindings, &allowed).unwrap();
    let references: BTreeSet<_> = index
        .authorized_capabilities(&allowed)
        .into_iter()
        .filter(|r| names.iter().any(|name| name.as_str() == r.capability))
        .collect();
    assert_eq!(references.len(), 3);
    let (recall, _) = index
        .evidence("g", &references, &catalogs, &bindings, &allowed)
        .unwrap();
    let packet = WholeIntentPacket::new(
        DEFAULT_MODEL,
        "Read the record.",
        &recall,
        &catalogs,
        &allowed,
        &index,
        &bindings,
        &BTreeMap::new(),
        &[],
    )
    .unwrap();
    assert!(packet.body.len() <= MAX_BODY_BYTES);
    assert_eq!(packet.candidates.len(), 2);
    assert_eq!(packet.omitted.len(), 1);
    assert_eq!(packet.omitted[0].capability, names[1].as_str());
    assert_eq!(packet.candidates[0].reference.capability, names[0].as_str());
    assert_eq!(packet.candidates[1].reference.capability, names[2].as_str());
    let body: Value = serde_json::from_str(&packet.body).unwrap();
    let content = &body["state"];
    assert_eq!(content["candidates"][0]["id"], "c0");
    assert_eq!(content["candidates"][1]["id"], "c1");
}

#[test]
fn parallel_text_keys_preserve_uncertainty_and_require_unambiguous_catalogue_reads() {
    let mut packet = packet();
    let mut cgs = plasm_core::loader::load_schema_dir(
        &std::path::Path::new(crate::test_manifest_dir())
            .join("../../fixtures/schemas/plasm_prompt_matrix"),
    )
    .unwrap();
    let mut other = cgs.clone();
    cgs.entities
        .get_mut("Zone")
        .unwrap()
        .discovery
        .get_or_insert_with(Default::default)
        .names
        .push("territory".into());
    other.entities.get_mut("Zone").unwrap().description = "An independent billing record".into();
    other.capabilities.get_mut("zone_get").unwrap().description =
        "Read a billing record with its territory.".into();
    let mut catalogs = BTreeMap::from([("first".into(), cgs), ("second".into(), other)]);
    for (c, catalog) in packet.candidates.iter_mut().zip(["first", "second"]) {
        c.reference.catalog = catalog.into();
        c.reference.capability = "zone_get".into();
        c.document.entity = "Zone".into();
        c.document.operation.kind = plasm_core::CapabilityKind::Get;
    }
    let target = packet.candidates[1].reference.clone();
    let assess = |intent: &str, catalogs: &BTreeMap<String, CGS>| {
        let mut selection = packet
            .decode(&response("relevant", "unrelated", ""))
            .unwrap();
        selection.apply(
            packet.candidates(),
            &References::bind(intent, catalogs),
            catalogs,
            &DeploymentBindings::default(),
        );
        selection.assessments[&target].clone()
    };
    let supported = "Read first territory east-2, and second zone west_1.";
    let lookup = References::record_reads(supported, &catalogs).unwrap();
    assert!(packet
        .candidates
        .iter()
        .all(|c| lookup.contains(&c.reference)));
    assert!(lookup.iter().all(|r| {
        let cap = &catalogs[&r.catalog].capabilities[r.capability.as_str()];
        cap.domain.as_str() == "Zone"
            && matches!(
                cap.kind,
                plasm_core::CapabilityKind::Get
                    | plasm_core::CapabilityKind::Query
                    | plasm_core::CapabilityKind::Search
            )
    }));
    let key = &catalogs["second"].entities["Zone"].id_field;
    assert_eq!(
        Some(lookup),
        References::record_reads(
            &format!(
                "Fetch first territory east and get second zone west by {key}. Return their records."
            ),
            &catalogs
        )
    );
    assert!(References::record_reads(
        "Read first zone east and second zone west by unknown.",
        &catalogs
    )
    .is_none());
    let result = assess(supported, &catalogs);
    assert_eq!(result.verdict, selection::Verdict::Exclude);
    assert_eq!(
        result.disposition,
        Disposition::Warn(selection::Warning::ParallelKey)
    );
    for intent in [
        "Read first territory east, then search for “accounts, second zone west, pending review”.",
        "Read first zone east and search for \"accounts, second zone west, pending review\".",
        "Read first zone east, exclude these records: first zone south, second zone west.",
        "Do not read first zone east, second zone west.",
        "Read first zone east, ignore second zone west.",
        "Read first zone east and second territory west.",
        "Read first zone east. Second zone west.",
    ] {
        assert!(
            References::record_reads(intent, &catalogs).is_none(),
            "{intent}"
        );
        assert_eq!(
            assess(intent, &catalogs).disposition,
            Disposition::Exclude,
            "{intent}"
        );
    }
    // Even an unissued alternate in the pinned catalogue makes this binding ambiguous.
    let second = catalogs.get_mut("second").unwrap();
    let mut alternate = second.capabilities["zone_get"].clone();
    alternate.name = "other_zone_get".into();
    second
        .capabilities
        .insert(alternate.name.clone(), alternate);
    assert!(References::record_reads(supported, &catalogs).is_none());
    assert_eq!(
        assess(supported, &catalogs).disposition,
        Disposition::Exclude
    );
}

#[test]
fn quote_markers_preserve_arguments_without_manufacturing_numeric_keys() {
    let intent = IntentText::parse("Get the store's entry “read X, then Y” and search for `α`.");
    assert_eq!(intent.literals, ["read X, then Y", "α"]);
    assert_eq!(
        intent.outer,
        "Get the store's entry ⟦literal1⟧ and search for ⟦literal2⟧."
    );
    let mut candidate = packet().candidates[0].clone();
    candidate.reference.catalog = "store".into();
    candidate.document.entity = "entry".into();
    candidate.document.operation.kind = plasm_core::CapabilityKind::Get;
    let request = IntentText::parse("Read Store entry 'not a number'.");
    assert!(!QualifiedSubjects::new(&request.outer).names_numbered_record(&candidate));
}

#[test]
fn qualified_subjects_distinguish_catalogue_role_and_numbered_entity() {
    let mut candidate = packet().candidates[0].clone();
    candidate.reference.catalog = "recordstore".into();
    candidate.document.entity = "Entry".into();
    candidate.document.operation.kind = plasm_core::CapabilityKind::Get;
    candidate.document.operation.contract = "Read one public listing.".into();
    let request = QualifiedSubjects::new("List other entries, then read Record Store listing.");
    assert!(request.qualified_roles(&candidate).next().is_some());
    assert!(!request.names_numbered_record(&candidate));
    let request = QualifiedSubjects::new("Read RecordStore entry 41.");
    assert!(request.names_numbered_record(&candidate));
    candidate.reference.catalog = "other".into();
    assert!(!request.qualified_roles(&candidate).next().is_some());
    assert!(!request.names_numbered_record(&candidate));
}

#[test]
fn complementary_read_admission_survives_collection_ranking_first() {
    let cgs = plasm_core::loader::load_schema_dir(
        &std::path::Path::new(crate::test_manifest_dir())
            .join("../../fixtures/schemas/whole_intent_uncertainty"),
    )
    .unwrap();
    let catalogs = BTreeMap::from([("dispatch_fixture".into(), cgs)]);
    let mut collection = packet().candidates[0].clone();
    collection.reference = CapabilityRef {
        catalog: "dispatch_fixture".into(),
        capability: "shipment_list".into(),
    };
    collection.document.entity = "Shipment".into();
    collection.document.operation.kind = plasm_core::CapabilityKind::Query;
    let mut detail = collection.clone();
    detail.reference.capability = "shipment_weight".into();
    detail.document.entity = "Measurement".into();
    detail.document.operation.kind = plasm_core::CapabilityKind::Get;
    for candidates in [
        [collection.clone(), detail.clone()],
        [detail.clone(), collection.clone()],
    ] {
        let priorities = admission_preferences(
            "List shipments and their weights",
            &candidates,
            &catalogs,
            BTreeSet::new(),
        );
        assert_eq!(priorities, BTreeSet::from([detail.reference.clone()]));
    }
    let mut ambiguous = catalogs;
    let catalog = ambiguous.get_mut("dispatch_fixture").unwrap();
    let mut other = catalog.entities["Measurement"].clone();
    other.name = "OtherRecord".into();
    catalog.entities.insert(other.name.clone(), other);
    let priorities = admission_preferences(
        "List shipments and their weights",
        &[collection, detail],
        &ambiguous,
        BTreeSet::new(),
    );
    assert!(priorities.is_empty());
}

#[test]
fn exact_operation_admission_respects_qualification_and_literal_boundaries() {
    let mut first = packet().candidates[0].clone();
    first.reference.catalog = "alpha".into();
    first.reference.capability = "record_create".into();
    let mut second = first.clone();
    second.reference.catalog = "beta".into();
    let candidates = vec![first, second];
    let exposed = [candidates[0].reference.clone()];
    for (intent, expected) in [
        ("use beta/record_create.", "beta"),
        ("use record_create", "alpha"),
        ("use beta/record_create_extra", "alpha"),
        ("store \"beta/record_create\"", "alpha"),
    ] {
        let request = IntentText::parse(intent);
        let ordered = order_candidates(
            &candidates,
            &request.outer,
            &BTreeMap::new(),
            &exposed,
            &BTreeSet::new(),
        );
        assert_eq!(ordered[0].reference.catalog, expected, "{intent}");
    }
}

#[test]
fn read_family_promotion_preserves_catalogue_and_priority_boundaries() {
    let base = packet().candidates[0].clone();
    let mut query = base.clone();
    query.reference.catalog = "first".into();
    query.reference.capability = "query".into();
    query.document.entity = "Record".into();
    query.document.operation.kind = plasm_core::CapabilityKind::Query;
    let mut other_catalogue = base.clone();
    other_catalogue.reference.catalog = "second".into();
    other_catalogue.reference.capability = "other".into();
    let mut unrelated = base.clone();
    unrelated.reference.catalog = "first".into();
    unrelated.reference.capability = "unrelated".into();
    unrelated.document.entity = "Other".into();
    unrelated.document.operation.kind = plasm_core::CapabilityKind::Get;
    let mut sibling = base;
    sibling.reference.catalog = "first".into();
    sibling.reference.capability = "sibling".into();
    sibling.document.entity = "Record".into();
    sibling.document.operation.kind = plasm_core::CapabilityKind::Get;
    let candidates = vec![query, other_catalogue, unrelated, sibling];
    let order = order_candidates(&candidates, "", &BTreeMap::new(), &[], &BTreeSet::new());
    assert_eq!(
        order
            .iter()
            .map(|c| c.reference.capability.as_str())
            .collect::<Vec<_>>(),
        ["query", "other", "sibling", "unrelated"]
    );
    let aged = BTreeMap::from([(candidates[2].reference.clone(), 3)]);
    let order = order_candidates(&candidates, "", &aged, &[], &BTreeSet::new());
    assert_eq!(order[0].reference, candidates[2].reference);
    assert_eq!(order.len(), candidates.len());
}

#[test]
fn allocation_keeps_catalogue_opportunity_then_recall_order() {
    let base = packet().candidates[0].clone();
    let candidates: Vec<_> = [
        ("a", "first"),
        ("a", "second"),
        ("a", "third"),
        ("b", "first"),
        ("b", "second"),
        ("c", "first"),
    ]
    .into_iter()
    .map(|(catalog, capability)| {
        let mut item = base.clone();
        item.reference.catalog = catalog.into();
        item.reference.capability = capability.into();
        item
    })
    .collect();
    let preferred = candidates.iter().map(|c| c.reference.clone()).collect();
    let ordered = order_candidates(&candidates, "", &BTreeMap::new(), &[], &preferred);
    let names: Vec<_> = ordered
        .iter()
        .map(|c| {
            (
                c.reference.catalog.as_str(),
                c.reference.capability.as_str(),
            )
        })
        .collect();
    assert_eq!(
        names,
        vec![
            ("a", "first"),
            ("b", "first"),
            ("c", "first"),
            ("a", "second"),
            ("a", "third"),
            ("b", "second")
        ]
    );
    let aged = BTreeMap::from([(candidates[2].reference.clone(), 3)]);
    let advanced = order_candidates(&candidates, "", &aged, &[], &preferred);
    assert_eq!(advanced[0].reference, candidates[2].reference);
    let preferred = BTreeSet::from([candidates[1].reference.clone()]);
    let ordered = order_candidates(&candidates, "", &BTreeMap::new(), &[], &preferred);
    assert_eq!(ordered[0].reference, candidates[1].reference);
    let advanced = order_candidates(&candidates, "", &aged, &[], &preferred);
    assert_eq!(advanced[0].reference, candidates[2].reference);
}

#[test]
fn named_catalogues_get_distinct_access_paths_before_duplicate_operations() {
    use plasm_core::CapabilityKind::{Create, Get, Query, Search};
    let base = packet().candidates[0].clone();
    let candidates: Vec<_> = [
        ("alpha", Get),
        ("alpha", Get),
        ("alpha", Query),
        ("beta", Create),
        ("beta", Search),
        ("other", Get),
    ]
    .into_iter()
    .enumerate()
    .map(|(i, (catalog, kind))| {
        let mut candidate = base.clone();
        candidate.reference.catalog = catalog.into();
        candidate.reference.capability = format!("op_{i}");
        candidate.document.operation.kind = kind;
        candidate
    })
    .collect();
    let ordered = order_candidates(
        &candidates,
        "compare Alpha and Beta",
        &BTreeMap::new(),
        &[],
        &BTreeSet::new(),
    );
    let ids: Vec<_> = ordered
        .iter()
        .take(4)
        .map(|c| c.reference.capability.as_str())
        .collect();
    assert_eq!(ids, ["op_0", "op_2", "op_3", "op_4"]);
    assert_eq!(ordered.len(), candidates.len());
}

#[test]
fn abbreviated_parent_still_needs_the_child_noun_for_collection_admission() {
    let words = plasm_core::domain_lexicon::raw_words("WorkOrderAttachment");
    let mentions = |intent| {
        super::admission::mentions_collection(&super::admission::intent_terms(intent), &words)
    };
    assert!(mentions("enumerate attachments on WO 8"));
    assert!(!mentions("inspect WO 8"));
    assert!(!mentions("list all attachments"));
}

#[test]
fn matching_collection_and_receipt_returning_create_survive_projection_cutoff() {
    use plasm_core::CapabilityKind::{Create, Query};
    let base = packet().candidates[0].clone();
    let mut candidates: Vec<_> = (0..80)
        .map(|i| {
            let mut c = base.clone();
            c.reference.catalog = "alpha".into();
            c.reference.capability = format!("other_{i}");
            c.document.entity = "Other".into();
            c.document.operation.kind = Query;
            c
        })
        .collect();
    for (i, meaning) in [
        (0, "Create a support ticket."),
        (8, "Create a shared itinerary."),
    ] {
        candidates[i].reference.catalog = "beta".into();
        candidates[i].document.entity = "Receipt".into();
        candidates[i].document.operation.kind = Create;
        candidates[i].document.operation.contract = meaning.into();
    }
    candidates[79].document.entity = "Station".into();
    let ordered = order_candidates(
        &candidates,
        "Find stations in Alpha and create a shared itinerary in Beta.",
        &BTreeMap::new(),
        &[],
        &BTreeSet::new(),
    );
    let first: BTreeSet<_> = ordered
        .iter()
        .take(2)
        .map(|c| c.reference.capability.as_str())
        .collect();
    assert_eq!(first, BTreeSet::from(["other_8", "other_79"]));
}

#[test]
fn weak_catalogue_match_does_not_jump_ahead_of_stronger_ranked_candidates() {
    let base = packet().candidates[0].clone();
    let mut candidates: Vec<_> = (0..CATALOG_OPPORTUNITY_WINDOW)
        .map(|n| {
            let mut c = base.clone();
            c.reference.catalog = "strong".into();
            c.reference.capability = format!("cap_{n}");
            c
        })
        .collect();
    let mut weak = base.clone();
    weak.reference.catalog = "weak".into();
    weak.reference.capability = "weak_cap".into();
    candidates.push(weak);
    let ordered = order_candidates(&candidates, "", &BTreeMap::new(), &[], &BTreeSet::new());
    assert_eq!(
        ordered.last().unwrap().reference,
        candidates.last().unwrap().reference
    );
    assert_eq!(ordered.len(), candidates.len());
}

#[test]
fn overdue_recalled_candidate_advances_without_granting_selection() {
    let initial = packet_with_priority(false);
    let advanced = packet_with_priority(true);
    assert_eq!(initial.candidates.len(), 2);
    assert_eq!(
        advanced.candidates[0].reference,
        initial.candidates[1].reference
    );
    assert!(advanced
        .decode(&response("unrelated", "unrelated", "unresolved"))
        .unwrap()
        .assessments
        .values()
        .all(|a| a.disposition == Disposition::Exclude));
}

fn response(a: &str, b: &str, _coverage: &str) -> String {
    let answer = |choice: &str, choices: &[&str]| {
        let probabilities: Map<String, Value> = choices
            .iter()
            .map(|k| (k.to_string(), json!(if *k == choice { 1.0 } else { 0.0 })))
            .collect();
        json!({"type":"choice","choice":choice,"confidence":1.0,"probabilities":probabilities})
    };
    let map = |s| match s {
        "relevant" => "include",
        "unrelated" => "exclude",
        _ => "uncertain",
    };
    let value = json!({"model":DEFAULT_MODEL,"provider":"OpenAI","answers":{"c0":answer(map(a), &["include","exclude","uncertain","uncertain_unit","uncertain_output","uncertain_constraint","uncertain_input"]),"c1":answer(map(b), &["include","exclude","uncertain","uncertain_unit","uncertain_output","uncertain_constraint","uncertain_input"])},"usage":{"input_tokens":42}});
    value.to_string()
}
#[test]
fn candidate_question_preserves_selection_contract() {
    let mut cards = vec![json!({"id":"c0","catalog":"fixture","entity":"Record",
        "capability":"records_query","operation":{"kind":"query","contract":"Find records."},
        "declaration":{"selection":[{"name":"owner","description":"Filter by owner.","required":false,"selection_effect":"filter"}],"provides":["id"]}})];
    cards.push(json!({"id":"c1","catalog":"fixture","entity":"Record",
        "capability":"record_edit","operation":{"kind":"update","contract":"Change a record."},
        "declaration":{"provides":["id"]}}));
    let records = Map::from_iter([("fixture/Record".into(), json!({"description":"A record."}))]);
    let evidence = super::super::evidence::CatalogEvidence::default();
    let (body, count) = request_body(
        EvidenceScope::WholeIntent,
        DEFAULT_MODEL,
        "Find records.",
        &cards,
        &[
            AccessQuestion::Read("Does the request need this collection?"),
            AccessQuestion::Effect,
        ],
        &Map::new(),
        &records,
        &BTreeMap::new(),
        &evidence,
        &mut [],
    )
    .unwrap();
    let packet: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(count, 2);
    let questions = &packet["questions"];
    assert!(questions["c0"]["criteria"]["include"]
        .as_str()
        .unwrap()
        .contains("supplies an input"));
    assert!(questions["c1"]["criteria"]["include"]
        .as_str()
        .unwrap()
        .contains("requests this state change"));
    assert!(questions["c1"]["criteria"]["exclude"]
        .as_str()
        .unwrap()
        .contains("only wants to observe"));
    let instructions = packet["questions"]["c0"]["instructions"].as_str().unwrap();
    assert!(instructions.contains("Does the request need this collection?"));
    assert!(instructions.contains("selection:owner;optional;filter: Filter by owner."));
    assert!(instructions.contains("A record."));
    assert_eq!(
        packet["state"]["candidates"][0]["declaration"],
        cards[0]["declaration"]
    );
}

#[test]
fn request_contains_exactly_one_bound_decision_per_candidate() {
    let packet = packet();
    let value: Value = serde_json::from_str(packet.body()).unwrap();
    let questions = value["questions"].as_object().unwrap();
    assert_eq!(questions.len(), packet.candidates.len());
    for i in 0..packet.candidates.len() {
        let question = &questions[&format!("c{i}")];
        assert_eq!(question["type"], "choice");
        assert!(question["criteria"].get("uncertain_unit").is_some());
    }
    assert!(value["state"].get("fragments").is_none());
}

#[test]
fn composable_choices_preserve_each_candidates_uncertainty() {
    let packet = packet();
    let raw = response("relevant", "uncertain", "unresolved");
    let decoded = packet.decode(&raw).unwrap();
    assert_eq!(
        decoded.assessments[&packet.candidates[0].reference].disposition,
        Disposition::Teach
    );
    assert!(decoded.assessments[&packet.candidates[1].reference]
        .disposition
        .uncertain());
    assert_eq!(decoded.tokens, Some(42));
    let mut value: Value = serde_json::from_str(&raw).unwrap();
    value["answers"]["c0"]["confidence"] = json!(0.1);
    assert_eq!(
        packet
            .decode(&value.to_string())
            .unwrap()
            .assessments
            .values()
            .filter(|a| a.disposition.uncertain())
            .count(),
        2
    );
    value["answers"]["forged"] = value["answers"]["c0"].clone();
    assert!(packet.decode(&value.to_string()).is_err());
}

#[test]
fn shared_record_identity_cannot_promote_a_deferred_sibling() {
    let mut packet = packet();
    let graph = plasm_core::loader::load_schema_dir(
        &std::path::Path::new(crate::test_manifest_dir())
            .join("../../fixtures/schemas/plasm_prompt_matrix"),
    )
    .unwrap();
    let catalogs = BTreeMap::from([("matrix".into(), graph)]);
    for c in &mut packet.candidates {
        c.reference.catalog = "matrix".into();
        c.document.entity = "Zone".into();
    }
    packet.candidates[0].document.operation.kind = plasm_core::CapabilityKind::Get;
    packet.candidates[1].document.operation.kind = plasm_core::CapabilityKind::Query;
    assert!(record_identity(&packet.candidates[0], &catalogs).is_some());
    let mut value: Value = serde_json::from_str(&response("uncertain", "unrelated", "")).unwrap();
    value["answers"]["c0"]["choice"] = json!("uncertain_input");
    value["answers"]["c0"]["probabilities"]["uncertain_input"] = json!(1.0);
    value["answers"]["c0"]["probabilities"]["uncertain"] = json!(0.0);
    value["answers"]["c1"]["confidence"] = json!(0.59);
    let mut selection = packet.decode(&value.to_string()).unwrap();
    selection.apply(
        packet.candidates(),
        &References::bind("Read the zone.", &catalogs),
        &catalogs,
        &DeploymentBindings::default(),
    );
    let sibling = &selection.assessments[&packet.candidates[1].reference];
    assert_eq!(sibling.verdict, selection::Verdict::Exclude);
    assert_eq!(sibling.disposition, Disposition::Defer);
    assert_eq!(selection.gaps().len(), 1);
}

#[test]
fn weak_read_exclusions_do_not_promote_effects() {
    use plasm_core::CapabilityKind;
    let mut packet = packet();
    packet.candidates[0].document.operation.kind = CapabilityKind::Get;
    packet.candidates[1].document.operation.kind = CapabilityKind::Create;
    let mut value: Value =
        serde_json::from_str(&response("unrelated", "unrelated", "unresolved")).unwrap();
    for id in ["c0", "c1"] {
        value["answers"][id]["confidence"] = json!(0.59);
    }
    let mut decoded = packet.decode(&value.to_string()).unwrap();
    decoded.apply(
        packet.candidates(),
        &References::bind("", &BTreeMap::new()),
        &BTreeMap::new(),
        &DeploymentBindings::default(),
    );
    assert_eq!(
        decoded.assessments[&packet.candidates[0].reference].disposition,
        Disposition::Defer
    );
    assert_eq!(
        decoded.assessments[&packet.candidates[1].reference].disposition,
        Disposition::Exclude
    );
    value["answers"]["c0"]["confidence"] = json!(0.60);
    assert!(packet
        .decode(&value.to_string())
        .unwrap()
        .assessments
        .values()
        .all(|a| a.disposition == Disposition::Exclude));
}

#[test]
fn only_unique_uncertain_keyed_reads_are_taught_for_a_numbered_record() {
    let mut packet = packet();
    let raw = response("unrelated", "unrelated", "unresolved");
    let mut value: Value = serde_json::from_str(&raw).unwrap();
    value["answers"]["c0"]["confidence"] = json!(0.51);
    for candidate in &mut packet.candidates {
        candidate.document.entity = "Entry".into();
    }
    packet.candidates[0].document.operation.kind = plasm_core::CapabilityKind::Get;
    packet.candidates[1].document.operation.kind = plasm_core::CapabilityKind::Query;
    let raw = value.to_string();
    let request = format!("Read {} entry 81.", packet.candidates[0].reference.catalog);
    let classify = |packet: &WholeIntentPacket| {
        let mut selection = packet.decode(&raw).unwrap();
        selection.apply(
            packet.candidates(),
            &References::bind(&request, &BTreeMap::new()),
            &BTreeMap::new(),
            &DeploymentBindings::default(),
        );
        selection.assessments[&packet.candidates[0].reference].disposition
    };
    assert_eq!(
        classify(&packet),
        Disposition::Warn(selection::Warning::QualifiedKey)
    );
    packet.candidates[1].document.operation.kind = plasm_core::CapabilityKind::Get;
    assert_eq!(classify(&packet), Disposition::Defer);
}

#[test]
fn specific_uncertainty_is_bound_to_issued_candidate() {
    let packet = packet();
    let mut value: Value =
        serde_json::from_str(&response("relevant", "uncertain", "unresolved")).unwrap();
    value["answers"]["c1"]["choice"] = json!("uncertain_unit");
    value["answers"]["c1"]["probabilities"]["uncertain"] = json!(0.0);
    value["answers"]["c1"]["probabilities"]["uncertain_unit"] = json!(1.0);
    let selection = packet.decode(&value.to_string()).unwrap();
    assert_eq!(
        selection.gaps(),
        [format!(
            "measurement unit for {}/{}",
            packet.candidates[1].reference.catalog, packet.candidates[1].reference.capability
        )]
    );
}

#[test]
fn rejects_unbound_provider_and_duplicate_response_fields() {
    let packet = packet();
    let response = response("relevant", "unrelated", "supported");
    let mut value: Value = serde_json::from_str(&response).unwrap();
    value["model"] = json!("unrelated/model");
    assert!(packet.decode(&value.to_string()).is_err());
    value["model"] = json!(DEFAULT_MODEL);
    value["provider"] = json!("");
    assert!(packet.decode(&value.to_string()).is_err());
    let duplicate = response.replacen("\"model\":", "\"model\":\"other\",\"model\":", 1);
    assert!(packet.decode(&duplicate).is_err());
}

#[test]
fn keyed_packet_retains_contract_domains_and_uncertainty_choices_once() {
    let declaration = json!({"scope":[{"name":"shelf","required":true,"value_ref":"shelf_type"}],"provides":["name"]});
    let cards = [
        json!({"id":"c0","catalog":"matrix","entity":"Entry","capability":"entry_get","operation":{"kind":"get"},"declaration":declaration}),
    ];
    let record = json!({"description":"Catalogue entry","id_field":"name","fields":{"history":"irrelevant output schema"}});
    let domain = json!({"allowed_values":["current","archive"]});
    let (body, _) = request_body(
        EvidenceScope::KeyedRecords,
        DEFAULT_MODEL,
        "Get Matrix entry acme.",
        &cards,
        &[AccessQuestion::Read("Is this lookup needed?")],
        &Map::from_iter([("matrix/shelf_type".into(), domain.clone())]),
        &Map::from_iter([("matrix/Entry".into(), record)]),
        &BTreeMap::new(),
        &Default::default(),
        &mut [],
    )
    .unwrap();
    let packet: Value = serde_json::from_str(&body).unwrap();
    let question = &packet["questions"]["c0"];
    let (_, evidence) = question["instructions"]
        .as_str()
        .unwrap()
        .split_once(" Catalogue data: ")
        .unwrap();
    let evidence: Value = serde_json::from_str(evidence).unwrap();
    assert_eq!(evidence["declaration"], declaration);
    assert_eq!(evidence["id_field"], "name");
    assert_eq!(question["criteria"].as_object().unwrap().len(), 7);
    assert_eq!(
        packet["state"]["input_domains"]["matrix/shelf_type"],
        domain
    );
    assert!(packet["state"].get("candidates").is_none());
    assert!(!body.contains("irrelevant output schema"));
}

#[test]
fn projected_composite_lookup_constrains_selection_only_for_complete_reads() {
    let schema = plasm_core::loader::load_schema_dir(
        &std::path::Path::new(crate::test_manifest_dir())
            .join("../../fixtures/schemas/plasm_prompt_matrix"),
    )
    .unwrap();
    let catalogs = BTreeMap::from([("matrix".into(), schema)]);
    let reads = |intent| References::record_reads(intent, &catalogs);
    let expected = BTreeSet::from([CapabilityRef {
        catalog: "matrix".into(),
        capability: "ruleset_get".into(),
    }]);
    assert_eq!(
        reads("Read matrix ruleset east/primary and return its name."),
        Some(expected.clone())
    );
    assert_eq!(
        reads("Read matrix ruleset east/primary. Return name and description."),
        Some(expected)
    );
    for request in [
        "Read matrix ruleset east/primary and return missing.",
        "Read matrix ruleset east/primary and return name and then update it.",
        "Read matrix ruleset east/primary and replace it. Return name.",
        "Read matrix zone east/primary and return name.",
        "Read matrix ruleset east/primary/extra and return name.",
        "Read matrix ruleset east/ and return name.",
    ] {
        assert!(reads(request).is_none(), "{request}");
    }
}
