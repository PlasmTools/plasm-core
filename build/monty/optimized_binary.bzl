"""Preserve the worker's optimized runtime profile for every consumer."""

def _optimized_impl(_settings, _attr):
    return {"//command_line_option:compilation_mode": "opt"}

_optimized = transition(
    implementation = _optimized_impl,
    inputs = [],
    outputs = ["//command_line_option:compilation_mode"],
)

def _binary_impl(ctx):
    binary = ctx.executable.binary
    output = ctx.actions.declare_file(ctx.label.name)
    ctx.actions.symlink(output = output, target_file = binary, is_executable = True)
    runfiles = ctx.runfiles(files = [output, binary]).merge(ctx.attr.binary[0][DefaultInfo].default_runfiles)
    return [DefaultInfo(executable = output, runfiles = runfiles)]

optimized_binary = rule(
    implementation = _binary_impl,
    executable = True,
    attrs = {
        "binary": attr.label(executable = True, cfg = _optimized, mandatory = True),
        "_allowlist_function_transition": attr.label(default = "@bazel_tools//tools/allowlists/function_transition_allowlist"),
    },
)
