use crate::constants::{DEFAULT_MAX_ITEMS, DEFAULT_MIN_ITEMS, DEFAULT_PORT};

#[derive(Debug, PartialEq, Clone)]
pub enum CorsOrigins {
    All,
    List(Vec<String>),
}

impl std::str::FromStr for CorsOrigins {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "*" {
            Ok(CorsOrigins::All)
        } else {
            Ok(CorsOrigins::List(
                s.split(',').map(|o| o.to_string()).collect(),
            ))
        }
    }
}

#[derive(clap::Parser)]
pub struct Args {
    /// If running in a Docker container, this port number will be internal to the container.
    /// In that case, to expose Hermit on your desired port on the host machine,
    /// use Docker's port mapping (e.g. `docker run -p 9876:8532 hermit` to expose on port 9876).
    #[arg(long, default_value_t = DEFAULT_PORT, env = "HERMIT_PORT")]
    pub port: u16,

    /// A directory containing OpenAPI spec files to load.
    /// The directory must exist and contain valid OpenAPI spec files only
    #[arg(long, env = "HERMIT_SPECS_DIR", conflicts_with = "specs")]
    pub specs_dir: Option<std::path::PathBuf>,

    /// One or more OpenAPI spec files to load.
    /// Can be specified multiple times, as a space-separated list, or via HERMIT_SPECS (comma-separated).
    #[arg(long, num_args = 0.., env = "HERMIT_SPECS", value_delimiter = ',', conflicts_with = "specs_dir")]
    pub specs: Vec<std::path::PathBuf>,

    /// The minimum number of items to generate for array schemas.
    /// Must not be greater than `--max-items`.
    #[arg(long, default_value_t = DEFAULT_MIN_ITEMS, env = "HERMIT_MIN_ITEMS")]
    pub min_items: usize,

    /// The maximum number of items to generate for array schemas.
    /// Must not be less than `--min-items`.
    #[arg(long, default_value_t = DEFAULT_MAX_ITEMS, env = "HERMIT_MAX_ITEMS")]
    pub max_items: usize,

    /// Use `example` values from the spec instead of generating random data.
    #[arg(long, default_value_t = false, env = "HERMIT_USE_EXAMPLES")]
    pub use_examples: bool,

    /// Allowed CORS origins. Use `*` to allow all origins, or a comma-separated list of specific origins.
    #[arg(long, default_value = "*", env = "HERMIT_CORS_ALLOWED_ORIGINS")]
    pub cors_allowed_origins: CorsOrigins,
}

impl Args {
    pub fn validate(&self) -> Result<(), String> {
        if self.min_items > self.max_items {
            return Err(format!(
                "--min-items ({}) must not exceed --max-items ({})",
                self.min_items, self.max_items
            ));
        }
        if self.specs.is_empty() && self.specs_dir.is_none() {
            return Err("either --specs or --specs-dir must be provided".to_string());
        }
        if let Some(dir) = &self.specs_dir {
            if !dir.exists() {
                return Err(format!("--specs-dir '{}' does not exist", dir.display()));
            }
            if !dir.is_dir() {
                return Err(format!(
                    "--specs-dir '{}' is not a directory",
                    dir.display()
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Args, CorsOrigins};
    use crate::constants::{DEFAULT_MAX_ITEMS, DEFAULT_MIN_ITEMS};

    #[test]
    fn min_items_and_max_items_have_correct_defaults() {
        let args = Args::try_parse_from(["hermit", "--specs", "a.yaml"]).unwrap();
        assert_eq!(args.min_items, DEFAULT_MIN_ITEMS);
        assert_eq!(args.max_items, DEFAULT_MAX_ITEMS);
    }

    #[test]
    fn min_items_and_max_items_can_be_configured_via_cli() {
        let args = Args::try_parse_from([
            "hermit",
            "--specs",
            "a.yaml",
            "--min-items",
            "5",
            "--max-items",
            "10",
        ])
        .unwrap();
        assert_eq!(args.min_items, 5);
        assert_eq!(args.max_items, 10);
    }

    #[test]
    fn multiple_specs_are_accepted() {
        let args =
            Args::try_parse_from(["hermit", "--specs", "a.yaml", "b.yaml", "c.yaml"]).unwrap();
        assert_eq!(args.specs.len(), 3);
    }

    #[test]
    fn validate_rejects_min_greater_than_default_max() {
        let min = (DEFAULT_MAX_ITEMS + 1).to_string();
        let args =
            Args::try_parse_from(["hermit", "--specs", "a.yaml", "--min-items", &min]).unwrap();
        assert!(args.validate().is_err());
    }

    #[test]
    fn validate_rejects_max_less_than_default_min() {
        let max = (DEFAULT_MIN_ITEMS - 1).to_string();
        let args =
            Args::try_parse_from(["hermit", "--specs", "a.yaml", "--max-items", &max]).unwrap();
        assert!(args.validate().is_err());
    }

    #[test]
    fn validate_rejects_min_greater_than_max() {
        let args = Args::try_parse_from([
            "hermit",
            "--specs",
            "a.yaml",
            "--min-items",
            "10",
            "--max-items",
            "3",
        ])
        .unwrap();
        assert!(args.validate().is_err());
    }

    #[test]
    fn validate_accepts_equal_min_and_max() {
        let args = Args::try_parse_from([
            "hermit",
            "--specs",
            "a.yaml",
            "--min-items",
            "5",
            "--max-items",
            "5",
        ])
        .unwrap();
        assert!(args.validate().is_ok());
    }

    #[test]
    fn validate_accepts_max_below_default_min_when_min_is_also_set_lower() {
        let val = (DEFAULT_MIN_ITEMS - 1).to_string();
        let args = Args::try_parse_from([
            "hermit",
            "--specs",
            "a.yaml",
            "--min-items",
            &val,
            "--max-items",
            &val,
        ])
        .unwrap();
        assert!(args.validate().is_ok());
    }

    #[test]
    fn use_examples_defaults_to_false() {
        let args = Args::try_parse_from(["hermit", "--specs", "a.yaml"]).unwrap();
        assert!(!args.use_examples);
    }

    #[test]
    fn use_examples_flag_can_be_set() {
        let args = Args::try_parse_from(["hermit", "--specs", "a.yaml", "--use-examples"]).unwrap();
        assert!(args.use_examples);
    }

    #[test]
    fn validate_accepts_min_above_default_max_when_max_is_also_set_higher() {
        let min = (DEFAULT_MAX_ITEMS + 1).to_string();
        let max = (DEFAULT_MAX_ITEMS + 2).to_string();
        let args = Args::try_parse_from([
            "hermit",
            "--specs",
            "a.yaml",
            "--min-items",
            &min,
            "--max-items",
            &max,
        ])
        .unwrap();
        assert!(args.validate().is_ok());
    }

    #[test]
    fn specs_dir_is_accepted() {
        let dir_name = "specs_assets";
        let args = Args::try_parse_from(["hermit", "--specs-dir", dir_name]).unwrap();
        assert_eq!(args.specs_dir, Some(std::path::PathBuf::from(dir_name)));
    }

    #[test]
    fn specs_dir_and_specs_are_mutually_exclusive() {
        assert!(
            Args::try_parse_from(["hermit", "--specs", "a.yaml", "--specs-dir", "some/dir"])
                .is_err()
        );
    }

    #[test]
    fn validate_rejects_when_neither_specs_nor_specs_dir_is_provided() {
        let args = Args::try_parse_from(["hermit"]).unwrap();
        assert!(args.validate().is_err());
    }

    #[test]
    fn validate_accepts_specs_dir_that_exists() {
        let args = Args::try_parse_from(["hermit", "--specs-dir", "specs_assets"]).unwrap();
        assert!(args.validate().is_ok());
    }

    #[test]
    fn validate_rejects_specs_dir_that_does_not_exist() {
        let args =
            Args::try_parse_from(["hermit", "--specs-dir", "/nonexistent/path/to/specs"]).unwrap();
        assert!(args.validate().is_err());
    }

    #[test]
    fn validate_rejects_specs_dir_pointing_to_a_non_directory_element() {
        let args =
            Args::try_parse_from(["hermit", "--specs-dir", "specs_assets/taskflow.openapi.yml"])
                .unwrap();
        assert!(args.validate().is_err());
    }

    // --- CORS allowed origins ---

    #[test]
    fn cors_allowed_origins_defaults_to_wildcard() {
        let args = Args::try_parse_from(["hermit", "--specs", "a.yaml"]).unwrap();
        assert_eq!(
            args.cors_allowed_origins,
            CorsOrigins::All,
            "cors_allowed_origins should default to CorsOrigins::All"
        );
    }

    #[test]
    fn cors_allowed_origins_accepts_wildcard_explicitly() {
        let args =
            Args::try_parse_from(["hermit", "--specs", "a.yaml", "--cors-allowed-origins", "*"])
                .unwrap();
        assert_eq!(
            args.cors_allowed_origins,
            CorsOrigins::All,
            "cors_allowed_origins should be CorsOrigins::All when * is passed"
        );
    }

    #[test]
    fn cors_allowed_origins_accepts_a_list() {
        let args = Args::try_parse_from([
            "hermit",
            "--specs",
            "a.yaml",
            "--cors-allowed-origins",
            "http://localhost:3000,http://example.com",
        ])
        .unwrap();
        assert_eq!(
            args.cors_allowed_origins,
            CorsOrigins::List(vec![
                "http://localhost:3000".to_string(),
                "http://example.com".to_string(),
            ]),
            "cors_allowed_origins should be CorsOrigins::List with the given origins"
        );
    }
}
