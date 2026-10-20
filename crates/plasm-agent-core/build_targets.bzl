load("@crates//:defs.bzl", "aliases", "all_crate_deps")
load("@rules_rust//cargo:defs.bzl", "cargo_build_script")
load("@rules_rust//rust:defs.bzl", "rust_library")
load("//build/bazel:native_test.bzl", rust_test = "rust_test_with_monty")

def build_targets():
    cargo_build_script(
        name = "build_script",
        srcs = ["build.rs"],
        build_script_env = {"PLASM_RELEASE_VERSION": "0.5.5"},
        data = native.glob(["src/assets/**"]),
        edition = "2021",
        version = "0.5.5",
    )

    _COMMON = dict(
        srcs = native.glob(["src/**/*.rs"]),
        aliases = aliases(),
        compile_data = native.glob([
            "src/assets/**",
            "src/**/*.sql",
            "src/**/*.txt",
            "migrations/**",
        ]),
        crate_root = "src/lib.rs",
        crate_features = select({
            "//:local_env_bootstrap": ["local_env_bootstrap_secrets"],
            "//conditions:default": [],
        }),
        edition = "2021",
        proc_macro_deps = all_crate_deps(proc_macro = True),
        version = "0.5.5",
        visibility = ["//visibility:public"],
        deps = [
            ":build_script",
            "//crates/plasm-catalog:plasm_catalog",
            "//crates/plasm-compile:plasm_compile",
            "//crates/plasm-core:plasm_core",
            "//crates/plasm-evidence:plasm_evidence",
            "//crates/plasm-runtime:plasm_runtime",
            "//crates/plasm-trace:plasm_trace",
            "//crates/plasm-trace-wire:plasm_trace_wire",
        ] + all_crate_deps(normal = True),
    )


    rust_library(
        name = "plasm_agent_core",
        **dict(_COMMON, deps = _COMMON["deps"] + ["//crates/plasm-otel:plasm_otel"])
    )

    rust_test(
        name = "unit_tests",
        **dict(
            _COMMON,
            crate_name = "plasm_agent_core",
            compile_data = _COMMON["compile_data"] + ["Cargo.toml", "//:vendor/README.md", "//:scripts/ci/build-monty-runtime.sh", "//:test_fixtures", "//:agent_prompt_assets"],
            data = ["Cargo.toml", "//:test_fixtures", "//:agent_prompt_assets"] + native.glob(["src/**/*.snap", "src/**/*.rs", "tests/support/**", "migrations/**"], allow_empty = True),
            env = {"INSTA_WORKSPACE_ROOT": "."},
            proc_macro_deps = _COMMON["proc_macro_deps"] + all_crate_deps(proc_macro_dev = True),
            deps = _COMMON["deps"] + ["//crates/plasm-otel:plasm_otel_testing"] + all_crate_deps(normal_dev = True),
        )
    )
