"""Resolve shared third-party crates from core's dependency graph."""

load("@crates//:defs.bzl", "crate_deps")

def core_crates(names):
    return crate_deps(names, package_name = "crates/plasm-agent-core")
