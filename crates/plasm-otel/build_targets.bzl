load("@crates//:defs.bzl", "aliases", "all_crate_deps")
load("@rules_rust//rust:defs.bzl", "rust_library")

def build_targets():
    _COMMON = dict(
        srcs = native.glob(["src/**/*.rs"]),
        aliases = aliases(),
        crate_root = "src/lib.rs",
        edition = "2021",
        proc_macro_deps = all_crate_deps(proc_macro = True),
        version = "0.5.5",
        visibility = ["//visibility:public"],
        deps = [] + all_crate_deps(normal = True),
    )

    rust_library(
        name = "plasm_otel",
        **_COMMON
    )

    rust_library(
        name = "plasm_otel_testing",
        crate_name = "plasm_otel",
        testonly = True,
        crate_features = ["testing"],
        **_COMMON
    )
