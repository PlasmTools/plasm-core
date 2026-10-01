use indexmap::IndexMap;
use plasm_compile::DecodedRelation;
use plasm_core::{Ref, Value};
use plasm_runtime::{CachedEntity, EntityCompleteness, GraphCache};

fn main() {
    println!("🗂️  STEP 6: Demonstrate Graph Cache System\n");

    let mut cache = GraphCache::new();

    // Create some entities with relations
    let account_ref = Ref::new("Account", "acc-1");
    let contact1_ref = Ref::new("Contact", "c-1");
    let contact2_ref = Ref::new("Contact", "c-2");

    // Create Account entity
    let mut account_fields = IndexMap::new();
    account_fields.insert("name".to_string(), Value::String("Acme Corp".to_string()));
    account_fields.insert("revenue".to_string(), Value::Float(1500.0));
    account_fields.insert("region".to_string(), Value::String("EMEA".to_string()));

    let mut account_relations = IndexMap::new();
    account_relations.insert(
        "contacts".to_string(),
        DecodedRelation::Specified(vec![contact1_ref.clone(), contact2_ref.clone()].into()),
    );

    let account = CachedEntity::from_decoded(
        account_ref.clone(),
        account_fields,
        account_relations,
        1,
        EntityCompleteness::Complete,
    );

    // Create Contact entities
    let mut contact1_fields = IndexMap::new();
    contact1_fields.insert(
        "name".to_string(),
        Value::String("John Manager".to_string()),
    );
    contact1_fields.insert("role".to_string(), Value::String("Manager".to_string()));

    let contact1 = CachedEntity::from_decoded(
        contact1_ref.clone(),
        contact1_fields,
        IndexMap::new(),
        1,
        EntityCompleteness::Complete,
    );

    let mut contact2_fields = IndexMap::new();
    contact2_fields.insert(
        "name".to_string(),
        Value::String("Jane Employee".to_string()),
    );
    contact2_fields.insert("role".to_string(), Value::String("Employee".to_string()));

    let contact2 = CachedEntity::from_decoded(
        contact2_ref.clone(),
        contact2_fields,
        IndexMap::new(),
        1,
        EntityCompleteness::Complete,
    );

    // Insert into cache
    println!("6a. Inserting entities into graph cache:");
    cache.insert(account.clone()).unwrap();
    cache.insert(contact1.clone()).unwrap();
    cache.insert(contact2.clone()).unwrap();

    let stats = cache.stats();
    println!(
        "   Cache stats: {} entities, {} types, version {}",
        stats.total_entities, stats.entity_types, stats.version
    );

    // Demonstrate stable identity
    println!("\n6b. Stable entity identity:");
    let retrieved_account = cache.get(&account_ref).unwrap();
    println!("   Account ref: {}", retrieved_account.reference);
    println!("   Account name: {:?}", retrieved_account.get_field("name"));

    // Demonstrate relation traversal
    println!("\n6c. Graph relation traversal:");
    if let Some(contact_refs) = retrieved_account.get_relations("contacts") {
        println!("   Account has {} contacts:", contact_refs.len());
        for contact_ref in contact_refs {
            if let Some(contact) = cache.get(contact_ref) {
                println!("     - {}: {:?}", contact_ref, contact.get_field("name"));
            }
        }
    }

    // Demonstrate type-based queries
    println!("\n6d. Type-based entity queries:");
    let accounts = cache.get_entities_by_type("Account");
    let contacts = cache.get_entities_by_type("Contact");
    println!("   Accounts in cache: {}", accounts.len());
    println!("   Contacts in cache: {}", contacts.len());

    println!("\n✓ Graph cache system verified!");
}
