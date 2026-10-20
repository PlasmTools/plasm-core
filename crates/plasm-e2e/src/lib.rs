pub mod evm_support;

/// Resolve fixture paths from the test runner rather than a compiler sandbox.
pub fn test_manifest_dir() -> &'static str {
    static DIRECTORY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DIRECTORY.get_or_init(|| {
        let path =
            std::env::var("CARGO_MANIFEST_DIR").expect("test runner must set CARGO_MANIFEST_DIR");
        std::path::Path::new(&path)
            .canonicalize()
            .expect("test manifest directory must exist")
            .to_str()
            .expect("test manifest directory must be UTF-8")
            .to_owned()
    })
}
