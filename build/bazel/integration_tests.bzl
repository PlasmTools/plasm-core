"""Core integration test executables using the package's existing Cargo metadata."""

load("@crates//:defs.bzl", "aliases", "all_crate_deps")
load(":native_test.bzl", rust_test = "rust_test_with_monty")

def rust_integration_tests(tests, deps, compile_data = [], data = [], crate_features = [], runtime_tools = {}, cargo_deps = None):
    """Declare one libtest executable per Cargo integration target."""
    support_sources = native.glob(["tests/**/*.rs"], exclude = tests.values())
    for name, source in tests.items():
        rust_test(
            name = name + "_tests",
            srcs = [source] + support_sources,
            crate_root = source,
            crate_name = name,
            edition = "2021",
            aliases = aliases(),
            crate_features = crate_features,
            compile_data = compile_data + native.glob(["tests/**"]),
            data = data + ["Cargo.toml"] + native.glob(["tests/**"]),
            deps = deps + (all_crate_deps(normal = True, normal_dev = True) if cargo_deps == None else cargo_deps),
            proc_macro_deps = all_crate_deps(proc_macro = True, proc_macro_dev = True),
            visibility = ["//visibility:public"],
            env = {
                "INSTA_WORKSPACE_ROOT": ".",
            },
            size = "large",
            runtime_tools = runtime_tools,
            # Service state is not an action input. Cache compilation, never
            # reuse a raw integration-test success across service environments.
            tags = ["external"],
        )
