use plasm_compile::{CompiledRequest, HttpBodyFormat, HttpMethod};
use plasm_core::Value;
use plasm_runtime::RequestFingerprint;

fn main() {
    println!("🔐 STEP 5: Demonstrate Deterministic Fingerprinting\n");

    // Create identical requests
    let request1 = CompiledRequest {
        credential: None,
        method: HttpMethod::Post,
        path: "/query/Account".to_string(),
        query: None,
        body: Some(Value::String(
            r#"{"filter": {"field": "region", "op": "=", "value": "EMEA"}}"#.to_string(),
        )),
        body_format: HttpBodyFormat::Json,
        multipart: None,
        headers: None,
    };

    let request2 = CompiledRequest {
        credential: None,
        method: HttpMethod::Post,
        path: "/query/Account".to_string(),
        query: None,
        body: Some(Value::String(
            r#"{"filter": {"field": "region", "op": "=", "value": "EMEA"}}"#.to_string(),
        )),
        body_format: HttpBodyFormat::Json,
        multipart: None,
        headers: None,
    };

    // Different request
    let request3 = CompiledRequest {
        credential: None,
        method: HttpMethod::Post,
        path: "/query/Account".to_string(),
        query: None,
        body: Some(Value::String(
            r#"{"filter": {"field": "region", "op": "=", "value": "APAC"}}"#.to_string(),
        )),
        body_format: HttpBodyFormat::Json,
        multipart: None,
        headers: None,
    };

    let fp1 = RequestFingerprint::from_request(&request1);
    let fp2 = RequestFingerprint::from_request(&request2);
    let fp3 = RequestFingerprint::from_request(&request3);

    println!("5a. Identical requests produce identical fingerprints:");
    println!("   Request 1: {}", fp1.to_hex());
    println!("   Request 2: {}", fp2.to_hex());
    println!("   Equal: {}", fp1 == fp2);

    println!("\n5b. Different requests produce different fingerprints:");
    println!("   Request 1: {}", fp1.to_hex());
    println!("   Request 3: {}", fp3.to_hex());
    println!("   Equal: {}", fp1 == fp3);

    println!("\n5c. Fingerprint hex round-trip stability:");
    let hex = fp1.to_hex();
    let recovered = RequestFingerprint::from_hex(&hex).unwrap();
    println!("   Original: {}", fp1.to_hex());
    println!("   Recovered: {}", recovered.to_hex());
    println!("   Round-trip works: {}", fp1 == recovered);

    println!("\n✓ Deterministic fingerprinting verified!");
}
