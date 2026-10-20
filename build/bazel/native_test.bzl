"""Rust tests with an absolute, declared Monty runtime at execution time."""

load("@rules_rust//rust:defs.bzl", _rust_test = "rust_test")

NativeTestBinaryInfo = provider(fields = ["binary", "runtime_tools", "runtime_files"])

def _runfile(ctx, file):
    path = file.short_path
    return path[3:] if path.startswith("../") else ctx.workspace_name + "/" + path

def _native_test_impl(ctx):
    binary = ctx.executable.binary
    monty = ctx.executable.monty
    manifest = _runfile(ctx, ctx.file.manifest)
    package_dir = manifest.removesuffix("/Cargo.toml")
    workspace_dir = package_dir.removesuffix("/" + ctx.label.package)
    launcher = ctx.actions.declare_file(ctx.label.name)
    runtime_tools = {}
    runtime_files = {}
    tool_env = []
    for target, variable in ctx.attr.runtime_tools.items():
        tool = target[DefaultInfo].files_to_run.executable
        runtime_tools[tool] = variable
        tool_env.append('"%s=$runfiles/%s"' % (variable, _runfile(ctx, tool)))
    for target, variable in ctx.attr.runtime_files.items():
        files = target[DefaultInfo].files.to_list()
        if len(files) != 1:
            fail("Runtime file must identify exactly one file: " + str(target.label))
        runtime_files[files[0]] = variable
        tool_env.append('"%s=$runfiles/%s"' % (variable, _runfile(ctx, files[0])))
    ctx.actions.write(
        launcher,
        "#!/usr/bin/env bash\nset -euo pipefail\n" +
        'runfiles="$(cd "${RUNFILES_DIR:-$0.runfiles}" && pwd -P)"\n' +
        'export PLASM_MONTY_BINARY="$runfiles/%s"\n' % _runfile(ctx, monty) +
        'export CARGO_MANIFEST_DIR="$runfiles/%s"\n' % package_dir +
        'export INSTA_WORKSPACE_ROOT="$runfiles/%s"\n' % workspace_dir +
        'if [[ -n "${TESTBRIDGE_TEST_ONLY:-}" ]]; then set -- "$TESTBRIDGE_TEST_ONLY" "$@"; fi\n' +
        'exec env %s "$runfiles/%s" "$@"\n' % (" ".join(tool_env), _runfile(ctx, binary)),
        is_executable = True,
    )
    runfiles = ctx.runfiles(files = [binary, monty, ctx.file.manifest] + runtime_tools.keys() + runtime_files.keys())
    for target in [ctx.attr.binary, ctx.attr.monty]:
        runfiles = runfiles.merge(target[DefaultInfo].default_runfiles)
    for target in ctx.attr.runtime_tools:
        runfiles = runfiles.merge(target[DefaultInfo].default_runfiles)
    return [DefaultInfo(executable = launcher, runfiles = runfiles), NativeTestBinaryInfo(binary = binary, runtime_tools = runtime_tools, runtime_files = runtime_files)]

_native_test = rule(
    implementation = _native_test_impl,
    test = True,
    attrs = {
        "binary": attr.label(executable = True, cfg = "target", mandatory = True),
        "manifest": attr.label(allow_single_file = True, mandatory = True),
        "monty": attr.label(default = Label("//:monty"), executable = True, cfg = "target"),
        "runtime_tools": attr.label_keyed_string_dict(cfg = "target"),
        "runtime_files": attr.label_keyed_string_dict(allow_files = True),
    },
)

def rust_test_with_monty(name, visibility = None, size = "large", runtime_tools = {}, runtime_files = {}, tags = [], **kwargs):
    """Expose a raw Bazel test and the same executable to nextest."""
    _rust_test(name = name + "_binary", tags = ["manual"] + tags, **kwargs)
    _native_test(name = name, binary = ":" + name + "_binary", manifest = ":Cargo.toml", visibility = visibility, size = size, runtime_tools = runtime_tools, runtime_files = runtime_files, tags = tags)
