"""Deterministic OSS archives assembled from the selected target platform."""

def _oss_release_impl(ctx):
    archives = [ctx.actions.declare_file("%s-%s.tar.gz" % (name, ctx.attr.triple)) for name in ["plasm-appliance", "plasm", "plasm-cgs"]]
    checksums = ctx.actions.declare_file("SHA256SUMS-" + ctx.attr.triple)
    manifest = ctx.actions.declare_file(ctx.label.name + ".json")
    ctx.actions.write(manifest, json.encode({
        "archives": [output.path for output in archives],
        "checksums": checksums.path,
        "server": ctx.executable.server.path,
        "client": ctx.executable.client.path,
        "cgs": ctx.executable.cgs.path,
        "monty": ctx.executable.monty.path,
        "license": ctx.file.license.path,
        "catalogs": ctx.file.catalogs.path,
    }))
    ctx.actions.run(
        executable = ctx.executable._pack,
        arguments = [manifest.path],
        inputs = [manifest, ctx.executable.server, ctx.executable.client, ctx.executable.cgs, ctx.executable.monty, ctx.file.license, ctx.file.catalogs],
        outputs = archives + [checksums],
        mnemonic = "OssReleaseArchives",
    )
    return [DefaultInfo(files = depset(archives + [checksums]))]

oss_release = rule(
    implementation = _oss_release_impl,
    attrs = {
        "triple": attr.string(mandatory = True),
        "server": attr.label(default = "//crates/plasm-server:plasm-server", executable = True, cfg = "target"),
        "client": attr.label(default = "//crates/plasm:plasm", executable = True, cfg = "target"),
        "cgs": attr.label(default = "//crates/plasm-cli:plasm-cgs", executable = True, cfg = "target"),
        "monty": attr.label(default = "//:monty", executable = True, cfg = "target"),
        "license": attr.label(default = "//:licenses/monty-MIT.txt", allow_single_file = True),
        "catalogs": attr.label(default = "//:catalogs", allow_single_file = True),
        "_pack": attr.label(default = "//build/bazel:release_archive", executable = True, cfg = "exec"),
    },
)
