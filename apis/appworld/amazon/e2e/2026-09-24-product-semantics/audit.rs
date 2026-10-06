fn main() -> Result<(), Box<dyn std::error::Error>> {
 let mut cgs = plasm_core::load_schema_dir(std::path::Path::new("plasm-oss/apis/appworld/amazon"))?;
 cgs.bind_registry_entry_id("amazon");
 let bytes = plasm_core::catalog_il::cgs_to_catalog_il_bytes(&cgs)?;
 let decoded = plasm_core::catalog_il::load_catalog_il_bytes(&bytes)?;
 let docs = plasm_core::catalog_discovery::capability_documents(&cgs)?;
 assert_eq!(docs, plasm_core::catalog_discovery::capability_documents(&decoded)?);
 for doc in docs.iter().filter(|d| d.capability == "product_query" || d.capability == "product_get") {
  println!("DOCUMENT {}\n{}\n", doc.capability, doc.text);
  assert!(!doc.text.contains("Numeric amount or rating"));
  assert!(!doc.text.contains("text (optional)"));
 }
 let exposure = plasm_core::symbol_tuning::TeachingExposureSession::new(&decoded, "amazon", &["Product"]);
 let card = plasm_core::PromptPipelineConfig::default().render_teaching_first_wave_for_session(&decoded, &exposure, None);
 println!("TEACHING\n{card}");
 assert!(card.contains("Price of one unit"));
 assert!(card.contains("Product rating on a 0 to 5 scale"));
 assert!(card.contains("Seller rating on a 0 to 5 scale"));
 Ok(())
}
