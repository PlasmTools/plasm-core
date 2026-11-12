"""Public access to the core workspace's generated crate dependencies."""

load("@crates//:defs.bzl", _crate_deps = "crate_deps")

crate_deps = _crate_deps
