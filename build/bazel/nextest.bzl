"""Run nextest against test executables built by Bazel."""

load(":native_test.bzl", "NativeTestBinaryInfo")

def _runfile(ctx, file):
    path = file.short_path
    return path[3:] if path.startswith("../") else ctx.workspace_name + "/" + path

def _nextest_impl(ctx):
    launcher = ctx.actions.declare_file(ctx.label.name)
    toolchain = ctx.toolchains["@rules_rust//rust:toolchain_type"]
    if toolchain.target_triple == None or toolchain.target_triple.str != toolchain.exec_triple.str:
        fail("nextest requires native test executables; build cross-platform artifacts separately")
    runner = ctx.executable._runner

    arguments = []
    runfiles = ctx.runfiles(
        files = [runner, ctx.file.manifest, ctx.file._nextest, ctx.file._config],
        transitive_files = toolchain.all_files,
    )
    runfiles = runfiles.merge(ctx.attr._runner[DefaultInfo].default_runfiles)
    runtime_tools = {}
    runtime_files = {}
    for target, package in ctx.attr.binaries.items():
        binary = target[NativeTestBinaryInfo].binary if NativeTestBinaryInfo in target else target[DefaultInfo].files_to_run.executable
        arguments.append('--binary "%s=$runfiles/%s"' % (package, _runfile(ctx, binary)))
        runfiles = runfiles.merge(target[DefaultInfo].default_runfiles)
        runfiles = runfiles.merge(ctx.runfiles(files = [binary]))
        if NativeTestBinaryInfo in target:
            for tool, variable in target[NativeTestBinaryInfo].runtime_tools.items():
                if variable in runtime_tools and runtime_tools[variable] != tool:
                    fail("Conflicting runtime tools for " + variable)
                runtime_tools[variable] = tool
            for file, variable in target[NativeTestBinaryInfo].runtime_files.items():
                if variable in runtime_files and runtime_files[variable] != file:
                    fail("Conflicting runtime files for " + variable)
                runtime_files[variable] = file
    for target, variable in ctx.attr.runtime_tools.items():
        binary = target[DefaultInfo].files_to_run.executable
        if variable in runtime_tools and runtime_tools[variable] != binary:
            fail("Conflicting runtime tools for " + variable)
        runtime_tools[variable] = binary
        runfiles = runfiles.merge(target[DefaultInfo].default_runfiles)
        runfiles = runfiles.merge(ctx.runfiles(files = [binary]))
    for variable, binary in runtime_tools.items():
        arguments.append('--runtime-tool "%s=$runfiles/%s"' % (variable, _runfile(ctx, binary)))
    for variable, file in runtime_files.items():
        if variable in runtime_tools:
            fail("Runtime file and tool share variable " + variable)
        arguments.append('--runtime-file "%s=$runfiles/%s"' % (variable, _runfile(ctx, file)))
    ctx.actions.write(
        launcher,
        "#!/usr/bin/env bash\nset -euo pipefail\n" +
        'runfiles="${RUNFILES_DIR:-$0.runfiles}"\n' +
        'exec "$runfiles/%s" --manifest "$runfiles/%s" --nextest "$runfiles/%s" --cargo "$runfiles/%s" --rustc "$runfiles/%s" --config "$runfiles/%s" %s "$@"\n' % (
            _runfile(ctx, runner),
            _runfile(ctx, ctx.file.manifest),
            _runfile(ctx, ctx.file._nextest),
            _runfile(ctx, toolchain.cargo),
            _runfile(ctx, toolchain.rustc),
            _runfile(ctx, ctx.file._config),
            " ".join(arguments),
        ),
        is_executable = True,
    )
    return [DefaultInfo(executable = launcher, runfiles = runfiles)]

_nextest_test = rule(
    implementation = _nextest_impl,
    test = True,
    toolchains = ["@rules_rust//rust:toolchain_type"],
    attrs = {
        "binaries": attr.label_keyed_string_dict(mandatory = True),
        "runtime_tools": attr.label_keyed_string_dict(),
        "manifest": attr.label(allow_single_file = True, mandatory = True),
        "_runner": attr.label(default = Label("//build/bazel:nextest_runner"), executable = True, cfg = "target"),
        "_nextest": attr.label(default = Label("@nextest_runner//:cargo-nextest"), allow_single_file = True),
        "_config": attr.label(default = Label("//:.config/nextest.toml"), allow_single_file = True),
    },
)

def nextest(name, tags = [], **kwargs):
    """Keep the output-base lock throughout test execution and snapshot staging."""

    # Suites can use local services and write nextest reports into target/.
    # Reuse compiled actions, but always execute these environment-sensitive tests.
    _nextest_test(name = name, size = "enormous", tags = ["local", "external"] + tags, **kwargs)
