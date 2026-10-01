//! Bounded design demonstration, not integrated discovery routing.
//! Run: cargo run -p plasm-core --example relation_read_demo
//! Question: can selecting a relation expose its executable traversal without
//! selecting a child capability? This example covers complete parent embeddings only.
use plasm_core::{
    capability_exposure::selected_capability_surface,
    schema::RelationMaterialization,
    symbol_tuning::{ExposureEntityKey, ExposureSlotKey, TeachingExposureSession},
    ChainExpr, Expr, GetExpr, PromptPipelineConfig,
};

fn main() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/from_parent_get_nav");
    let mut cgs = plasm_core::load_schema_dir(&path).unwrap();
    cgs.bind_registry_entry_id("demo");

    // This is the decision a relation-aware selector would return. Reuse the
    // existing catalog-qualified slot type; no second graph or name inference.
    let selected = ExposureSlotKey::Relation {
        source: ExposureEntityKey {
            entry_id: "demo".into(),
            entity: "ParentItem".into(),
        },
        relation: "tags".into(),
    };
    let selected: ExposureSlotKey =
        serde_json::from_slice(&serde_json::to_vec(&selected).unwrap()).unwrap();
    let ExposureSlotKey::Relation { source, relation } = &selected else {
        unreachable!()
    };
    let parent = cgs.get_entity(source.entity.as_str()).unwrap();
    let edge = &parent.relations[relation];
    assert!(matches!(
        edge.materialize,
        Some(RelationMaterialization::FromParentGet { .. })
    ));
    let target = cgs.get_entity(edge.target_resource.as_str()).unwrap();
    let get = cgs.primary_get_capability(parent.name.as_str()).unwrap();
    assert!(cgs.primary_get_capability(target.name.as_str()).is_none());

    println!(
        "SELECTED: {}.{} -> {} ({:?})",
        parent.name, relation, target.name, edge.cardinality
    );
    println!("MEANING: {}", edge.description);
    println!("SOURCE: {}", parent.description);
    println!("TARGET: {}", target.description);
    println!("DERIVED READ DEPENDENCY: {}", get.name);
    let mut delta = selected_capability_surface(&cgs, "demo", &[get.name.to_string()]).unwrap();
    assert!(
        !delta.required.slots.contains(&selected),
        "baseline must exhibit the missing edge"
    );

    // New seam demonstrated: a selected relation admits its target row shape and
    // edge, not every capability owned by its target entity.
    let target_key = ExposureEntityKey {
        entry_id: "demo".into(),
        entity: target.name.clone(),
    };
    delta.required.entities.insert(target_key.clone());
    for field in target.fields.keys() {
        delta.required.slots.insert(ExposureSlotKey::EntityField {
            entity: target_key.clone(),
            field: field.clone(),
        });
    }
    delta.required.slots.insert(selected.clone());
    assert_eq!(delta.required.capabilities.len(), 1);
    assert!(delta
        .required
        .capabilities
        .iter()
        .all(|c| c.capability == get.name));
    println!("CAPABILITIES EXPOSED: parent Get only; child Query remains hidden");

    let exposure = TeachingExposureSession::new_with_intent_delta(
        &cgs,
        "demo",
        &[parent.name.as_str(), target.name.as_str()],
        delta,
    );
    let teaching = PromptPipelineConfig::default().render_teaching_exposure_delta(
        &cgs,
        &exposure,
        &[parent.name.as_str(), target.name.as_str()],
        None,
    );
    assert!(
        teaching.contains(".r1"),
        "relation must be rendered: {teaching}"
    );
    assert!(teaching.contains("e2 row[id,color,name]"));
    assert!(teaching.contains("Tag color") && teaching.contains("Tag name"));
    assert!(!teaching.contains("e2{") && !teaching.contains("e2("));
    println!("\nACTUAL TEACHING:\n{teaching}");

    let chain = ChainExpr::auto_get(
        Expr::Get(GetExpr::new(parent.name.as_str(), "p-1")),
        relation.as_str(),
    );
    let encoded = serde_json::to_vec(&chain).unwrap();
    let decoded: ChainExpr = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(chain, decoded);
    plasm_core::type_checker::type_check_chain(&decoded, &cgs).unwrap();
    println!("TYPED PROGRAM: ParentItem(\"p-1\").tags");
    println!("PASS: selected edge survives codec, renders, and type-checks without child Get/Query selection.");
}
