use std::path::Path;
use yaml_serde::Value;

use crate::spec_parser::{RouteConfig, extract_routes};

pub fn load(path: &Path) -> Value {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("{} should be readable: {e}", path.display()));
    yaml_serde::from_str(&text)
        .unwrap_or_else(|e| panic!("{} should contain valid YAML or JSON: {e}", path.display()))
}

pub fn load_dir(path: &Path) -> Vec<RouteConfig> {
    let files: Vec<std::path::PathBuf> = std::fs::read_dir(path)
        .expect("specs directory should be readable")
        .map(|e| e.expect("directory entry should be readable").path())
        .filter(|p| p.is_file())
        .collect();

    assert!(!files.is_empty(), "specs directory should contain files");

    load_all(&files)
}

pub fn load_all(paths: &[std::path::PathBuf]) -> Vec<RouteConfig> {
    let handles: Vec<_> = paths
        .iter()
        .map(|path| {
            let path = path.clone();
            std::thread::spawn(move || {
                let spec = load(&path);
                assert!(
                    spec.get("paths").is_some(),
                    "{} should be a valid OpenAPI spec",
                    path.display()
                );
                println!("Loaded spec: {}", path.display());
                extract_routes(&spec)
            })
        })
        .collect();

    handles
        .into_iter()
        .flat_map(|h| h.join().expect("spec loader thread should not panic"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_reads_yaml_from_file() {
        let val = load(Path::new("specs_assets/taskflow.openapi.yml"));
        assert!(val.get("paths").is_some());
    }

    #[test]
    #[should_panic(expected = "should contain valid YAML or JSON")]
    fn load_panics_on_invalid_yaml() {
        let path = std::env::temp_dir().join("hermit_test_invalid_yaml.txt");
        std::fs::write(&path, "key: {unclosed").unwrap();
        load(&path);
    }

    #[test]
    fn load_dir_returns_routes_from_all_files_in_directory() {
        let routes = load_dir(Path::new("specs_assets"));
        assert!(
            routes.len() >= 2,
            "expected routes from multiple spec files"
        );
    }

    #[test]
    #[should_panic]
    fn load_dir_panics_when_directory_is_empty() {
        let dir = std::env::temp_dir().join("hermit_test_empty_dir_for_load_dir");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("dummy"), "").unwrap();
        for entry in std::fs::read_dir(&dir).unwrap() {
            std::fs::remove_file(entry.unwrap().path()).ok();
        }
        load_dir(&dir);
    }

    #[test]
    #[should_panic]
    fn load_dir_panics_when_a_file_is_not_a_valid_openapi_spec() {
        let dir = std::env::temp_dir().join("hermit_test_invalid_spec_dir_for_load_dir");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("not_openapi.yaml"), "foo: bar").unwrap();
        load_dir(&dir);
    }
}
