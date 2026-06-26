//! Integration tests for [`super::detect_materialization_conflicts`] (CEP-11 / CEP-12 / CEP-14).

use super::*;
use crate::ExecutionSource;

fn type_entity_with_relation(relation: &str, target: Ref) -> crate::CachedEntity {
    use crate::{CachedEntity, EntityCompleteness};
    use indexmap::IndexMap;
    use plasm_core::{TypedFieldValue, Value};

    let reference = Ref::new("PokemonType", "electric");
    let mut fields = IndexMap::new();
    fields.insert(
        "name".to_string(),
        TypedFieldValue::from(Value::String("electric".into())),
    );
    let mut relations = IndexMap::new();
    relations.insert(relation.to_string(), vec![target]);
    CachedEntity {
        reference,
        fields,
        relations,
        last_updated: 1,
        version: 1,
        completeness: EntityCompleteness::Complete,
    }
}

#[test]
fn response_store_write_conflict_on_concurrent_insert() {
    let mut session = SessionResponseStore::default();
    let fp = RequestFingerprint::from_hex(&format!("{:064x}", 7u64)).expect("fp");
    session.store(
        fp.clone(),
        serde_json::json!({"a": 1}),
        ExecutionSource::Live,
    );

    let base = session.entries_snapshot();
    let mut branch = session.clone();
    branch.store(
        fp.clone(),
        serde_json::json!({"a": 2}),
        ExecutionSource::Live,
    );
    let write_set = branch.branch_write_fingerprints(&base);
    assert!(
        SessionResponseStore::detect_write_conflicts(&session, &branch, &base, &write_set)
            .is_empty()
    );

    session.store(
        fp.clone(),
        serde_json::json!({"a": 9}),
        ExecutionSource::Live,
    );
    let conflicts =
        SessionResponseStore::detect_write_conflicts(&session, &branch, &base, &write_set);
    assert_eq!(conflicts, vec![fp]);
}

#[test]
fn response_store_idempotent_concurrent_write_no_conflict() {
    let mut session = SessionResponseStore::default();
    let fp = RequestFingerprint::from_hex(&format!("{:064x}", 8u64)).expect("fp");
    session.store(
        fp.clone(),
        serde_json::json!({"a": 1}),
        ExecutionSource::Live,
    );

    let base = session.entries_snapshot();
    let mut branch = session.clone();
    branch.store(
        fp.clone(),
        serde_json::json!({"a": 2}),
        ExecutionSource::Live,
    );
    let write_set = branch.branch_write_fingerprints(&base);
    session.store(
        fp.clone(),
        serde_json::json!({"a": 2}),
        ExecutionSource::Live,
    );
    let conflicts =
        SessionResponseStore::detect_write_conflicts(&session, &branch, &base, &write_set);
    assert!(conflicts.is_empty());
}

#[test]
fn query_index_write_conflict_on_concurrent_key() {
    let mut session = QueryIndex::default();
    let key = QueryCacheKey::test("scoped\0label=1");
    let r1 = Ref::new("Label", "1");
    session.insert(key.clone(), vec![r1.clone()]);

    let base = session.entries_snapshot();
    let mut branch = session.clone();
    branch.insert(key.clone(), vec![Ref::new("Label", "2")]);
    let write_set = branch.branch_write_keys(&base);
    assert!(QueryIndex::detect_write_conflicts(&session, &branch, &base, &write_set).is_empty());

    session.insert(key.clone(), vec![Ref::new("Label", "9")]);
    let conflicts = QueryIndex::detect_write_conflicts(&session, &branch, &base, &write_set);
    assert_eq!(conflicts, vec![key]);
}

#[test]
fn query_index_idempotent_concurrent_write_no_conflict() {
    let mut session = QueryIndex::default();
    let key = QueryCacheKey::test("scoped\0label=2");
    let r1 = Ref::new("Label", "1");
    session.insert(key.clone(), vec![r1]);

    let base = session.entries_snapshot();
    let mut branch = session.clone();
    branch.insert(key.clone(), vec![Ref::new("Label", "2")]);
    let write_set = branch.branch_write_keys(&base);
    session.insert(key.clone(), vec![Ref::new("Label", "2")]);
    let conflicts = QueryIndex::detect_write_conflicts(&session, &branch, &base, &write_set);
    assert!(conflicts.is_empty());
}

#[test]
fn idempotent_shared_ref_no_conflict() {
    let mut session = SessionMaterialization::new();
    let electric = type_entity_with_relation("pokemon", Ref::new("Pokemon", "pikachu"));
    session.insert(electric.clone()).expect("seed");

    let (mut branch, base) = BranchMaterializationBase::fork_from(&session);
    let mut refreshed = electric.clone();
    refreshed.version = 99;
    refreshed.last_updated = 99;
    branch.insert(refreshed).expect("branch refresh");

    let conflicts = detect_materialization_conflicts(&session, &base, &branch);
    assert!(!conflicts.has_any());
}

#[test]
fn additive_disjoint_relation_keys_no_conflict() {
    let mut session = SessionMaterialization::new();
    let electric = type_entity_with_relation("pokemon", Ref::new("Pokemon", "pikachu"));
    session.insert(electric.clone()).expect("seed");

    let (mut branch_a, base_a) = BranchMaterializationBase::fork_from(&session);
    let (mut branch_b, base_b) = BranchMaterializationBase::fork_from(&session);

    let mut with_pokemon = electric.clone();
    with_pokemon
        .relations
        .insert("pokemon".into(), vec![Ref::new("Pokemon", "pikachu")]);
    with_pokemon.version = 2;
    branch_a.insert(with_pokemon).expect("branch a");

    let mut with_moves = electric.clone();
    with_moves
        .relations
        .insert("moves".into(), vec![Ref::new("Move", "thunderbolt")]);
    with_moves.version = 2;
    branch_b.insert(with_moves).expect("branch b");

    assert!(!detect_materialization_conflicts(&session, &base_a, &branch_a).has_any());
    session.absorb_branch(branch_a).expect("absorb branch a");

    assert!(!detect_materialization_conflicts(&session, &base_b, &branch_b).has_any());
    session.absorb_branch(branch_b).expect("absorb branch b");

    let live = session
        .get(&Ref::new("PokemonType", "electric"))
        .expect("electric");
    assert!(live.relations.contains_key("pokemon"));
    assert!(live.relations.contains_key("moves"));
}

#[test]
fn divergent_field_same_ref_is_conflict() {
    use plasm_core::{TypedFieldValue, Value};

    let mut session = SessionMaterialization::new();
    let electric = type_entity_with_relation("pokemon", Ref::new("Pokemon", "pikachu"));
    session.insert(electric.clone()).expect("seed");

    let (mut branch_a, base) = BranchMaterializationBase::fork_from(&session);
    let (mut branch_b, _) = BranchMaterializationBase::fork_from(&session);

    let mut a = electric.clone();
    a.fields.insert(
        "name".into(),
        TypedFieldValue::from(Value::String("branch_a".into())),
    );
    branch_a.insert(a).expect("branch a");

    let mut b = electric.clone();
    b.fields.insert(
        "name".into(),
        TypedFieldValue::from(Value::String("branch_b".into())),
    );
    branch_b.insert(b).expect("branch b");

    assert!(!detect_materialization_conflicts(&session, &base, &branch_a).has_any());
    session.absorb_branch(branch_a).expect("absorb branch a");

    let conflicts = detect_materialization_conflicts(&session, &base, &branch_b);
    assert_eq!(
        conflicts.graph_refs,
        vec![Ref::new("PokemonType", "electric")]
    );
}

#[test]
fn additive_disjoint_branches_confluent_absorb_order() {
    let mut session = SessionMaterialization::new();
    let electric = type_entity_with_relation("pokemon", Ref::new("Pokemon", "pikachu"));
    session.insert(electric.clone()).expect("seed");

    let (mut branch_a, _base) = BranchMaterializationBase::fork_from(&session);
    let (mut branch_b, _) = BranchMaterializationBase::fork_from(&session);

    let mut a = electric.clone();
    a.relations
        .insert("pokemon".into(), vec![Ref::new("Pokemon", "pikachu")]);
    branch_a.insert(a).expect("branch a");

    let mut b = electric.clone();
    b.relations
        .insert("moves".into(), vec![Ref::new("Move", "thunderbolt")]);
    branch_b.insert(b).expect("branch b");

    let mut forward = session.clone();
    forward.absorb_branch(branch_a.clone()).expect("a first");
    forward.absorb_branch(branch_b.clone()).expect("b second");

    let mut reverse = session;
    reverse.absorb_branch(branch_b).expect("b first");
    reverse.absorb_branch(branch_a).expect("a second");

    let ref_electric = Ref::new("PokemonType", "electric");
    let forward_live = forward.get(&ref_electric).expect("forward");
    let reverse_live = reverse.get(&ref_electric).expect("reverse");
    assert_eq!(forward_live.relations, reverse_live.relations);
}

#[test]
fn lazy_fork_base_scales_with_touch_set_not_session_size() {
    use crate::cache::GraphCache;
    use plasm_core::{TypedFieldValue, Value};

    let mut session = GraphCache::new();
    for i in 0..64 {
        let mut e = type_entity_with_relation("pokemon", Ref::new("Pokemon", "seed"));
        e.reference = Ref::new("Berry", format!("bulk-{i}"));
        e.fields = indexmap::IndexMap::from([(
            "name".into(),
            TypedFieldValue::from(Value::String(format!("bulk-{i}"))),
        )]);
        session.insert(e).expect("seed");
    }

    let mut branch = session.fork_for_branch();
    let touched = Ref::new("Berry", "bulk-0");
    let mut updated = session.get(&touched).expect("seed row").clone();
    updated.fields.insert(
        "name".into(),
        TypedFieldValue::from(Value::String("bulk-0-updated".into())),
    );
    branch.insert(updated).expect("touch one ref");

    let tracker = branch.branch_fork.as_ref().expect("tracker");
    assert_eq!(
        tracker.lazy_base.len(),
        1,
        "fork base must capture only mutated refs, not full session graph"
    );
}

proptest::proptest! {
    #[test]
    fn proptest_additive_relation_keys_never_conflict(
        pokemon_id in "[a-z]{3,8}",
        move_id in "[a-z]{3,8}",
    ) {
        let pokemon_id: String = pokemon_id;
        let move_id: String = move_id;
        let mut session = SessionMaterialization::new();
        let electric = type_entity_with_relation("pokemon", Ref::new("Pokemon", &pokemon_id));
        session.insert(electric.clone()).expect("seed");

        let (mut branch_a, base_a) = BranchMaterializationBase::fork_from(&session);
        let (mut branch_b, base_b) = BranchMaterializationBase::fork_from(&session);

        let mut a = electric.clone();
        a.relations.insert(
            "pokemon".into(),
            vec![Ref::new("Pokemon", pokemon_id.as_str())],
        );
        branch_a.insert(a).expect("branch a");

        let mut b = electric;
        b.relations.insert(
            "moves".into(),
            vec![Ref::new("Move", move_id.as_str())],
        );
        branch_b.insert(b).expect("branch b");

        proptest::prop_assert!(!detect_materialization_conflicts(&session, &base_a, &branch_a).has_any());
        session.absorb_branch(branch_a).expect("absorb a");
        proptest::prop_assert!(!detect_materialization_conflicts(&session, &base_b, &branch_b).has_any());
    }
}
