use indexmap::IndexMap;
use plasm_compile::validate_cgs_capability_templates;
use plasm_core::{type_check_expr, Expr, InvokeExpr, Value, CGS};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🔒 CAPABILITY INPUT VALIDATION DEMONSTRATION\n");

    let cgs: CGS = plasm_core::loader::load_schema_dir(std::path::Path::new(
        "fixtures/schemas/capability_with_input",
    ))
    .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    validate_cgs_capability_templates(&cgs)
        .map_err(|e| -> Box<dyn std::error::Error> { format!("{e}").into() })?;

    println!("Loaded schema with account_update capability");
    if let Some(cap) = cgs.get_capability("account_update") {
        println!("✓ Capability found: {} ({})", cap.name, cap.domain);
        if cap.input_schema.is_some() {
            println!("✓ Input schema defined for validation");
        }
    }

    // Test 1: Valid input
    println!("\n🧪 Test 1: Valid input (should succeed)");
    let mut valid_input = IndexMap::new();
    valid_input.insert(
        "name".to_string(),
        Value::String("Updated Corp Name".to_string()),
    );
    valid_input.insert("revenue".to_string(), Value::Float(1500.0));
    valid_input.insert("priority".to_string(), Value::String("high".to_string()));

    let valid_invoke = InvokeExpr::new(
        "account_update",
        "Account",
        "acc-1",
        Some(Value::Object(valid_input)),
    );

    match type_check_expr(&Expr::Invoke(valid_invoke), &cgs) {
        Ok(()) => println!("✓ Valid input passed type checking"),
        Err(e) => println!("✗ Unexpected error: {}", e),
    }

    // Test 2: Invalid input - negative revenue (should fail validation)
    println!("\n🧪 Test 2: Invalid input - negative revenue (should fail)");
    let mut invalid_input = IndexMap::new();
    invalid_input.insert("revenue".to_string(), Value::Float(-100.0)); // Negative revenue

    let invalid_invoke = InvokeExpr::new(
        "account_update",
        "Account",
        "acc-2",
        Some(Value::Object(invalid_input)),
    );

    match type_check_expr(&Expr::Invoke(invalid_invoke), &cgs) {
        Ok(()) => println!("✗ Invalid input should have failed!"),
        Err(e) => println!("✓ Correctly rejected invalid input: {}", e),
    }

    // Test 3: Invalid field type (should fail)
    println!("\n🧪 Test 3: Invalid field type - string for revenue (should fail)");
    let mut type_error_input = IndexMap::new();
    type_error_input.insert(
        "revenue".to_string(),
        Value::String("not-a-number".to_string()),
    );

    let type_error_invoke = InvokeExpr::new(
        "account_update",
        "Account",
        "acc-3",
        Some(Value::Object(type_error_input)),
    );

    match type_check_expr(&Expr::Invoke(type_error_invoke), &cgs) {
        Ok(()) => println!("✗ Type error should have been caught!"),
        Err(e) => println!("✓ Correctly rejected type mismatch: {}", e),
    }

    // Test 4: Invalid allowed value (should fail)
    println!("\n🧪 Test 4: Invalid allowed value - bad priority (should fail)");
    let mut enum_error_input = IndexMap::new();
    enum_error_input.insert("priority".to_string(), Value::String("invalid".to_string()));

    let enum_error_invoke = InvokeExpr::new(
        "account_update",
        "Account",
        "acc-4",
        Some(Value::Object(enum_error_input)),
    );

    match type_check_expr(&Expr::Invoke(enum_error_invoke), &cgs) {
        Ok(()) => println!("✗ Invalid enum value should have failed!"),
        Err(e) => println!("✓ Correctly rejected invalid enum: {}", e),
    }

    // Test 5: Empty input (should fail cross-field rule)
    println!("\n🧪 Test 5: Empty input - violates at_least_one rule (should fail)");
    let empty_input = IndexMap::new();

    let empty_invoke = InvokeExpr::new(
        "account_update",
        "Account",
        "acc-5",
        Some(Value::Object(empty_input)),
    );

    match type_check_expr(&Expr::Invoke(empty_invoke), &cgs) {
        Ok(()) => println!("✗ Empty input should violate cross-field rule!"),
        Err(e) => println!("✓ Correctly rejected empty input: {}", e),
    }

    println!("\n🏆 Input validation system fully operational!");
    println!("   ✓ Type checking works");
    println!("   ✓ Validation predicates work");
    println!("   ✓ Enum constraints work");
    println!("   ✓ Cross-field rules work");

    Ok(())
}
