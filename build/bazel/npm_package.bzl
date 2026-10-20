"""npm tarballs assembled from Bazel-built artifacts and canonical sources."""

def _npm_package_impl(ctx):
    output = ctx.actions.declare_file(ctx.label.package.split("/")[-1] + ("-" + ctx.attr.platform if ctx.attr.platform else "") + ".tgz")
    checksum = ctx.actions.declare_file(output.basename + ".sha256")
    manifest = ctx.actions.declare_file(ctx.label.name + ".inputs.json")
    prefix = ctx.label.package + "/"
    files = {}
    for file in ctx.files.srcs:
        relative = file.short_path.split("/", 2)[2] if file.short_path.startswith("../") else file.short_path
        files[relative.removeprefix(prefix)] = file.path
    inputs = ctx.files.srcs + [ctx.file.manifest, manifest]
    spec = {
        "manifest": ctx.file.manifest.path,
        "files": files,
        "version": ctx.var.get("plasm_release_version", ""),
        "platform": ctx.attr.platform,
        "output": output.path,
        "checksum": checksum.path,
    }
    if ctx.file.engine_manifest:
        spec["engine_manifest"] = ctx.file.engine_manifest.path
        inputs.append(ctx.file.engine_manifest)
    if ctx.file.ignore_file:
        spec["ignore_file"] = ctx.file.ignore_file.path
        inputs.append(ctx.file.ignore_file)
    if ctx.attr.platform:
        spec.update({"binding": ctx.file.binding.path, "monty": ctx.executable.monty.path, "monty_license": ctx.file.monty_license.path})
        inputs += [ctx.file.binding, ctx.executable.monty, ctx.file.monty_license]
    ctx.actions.write(manifest, json.encode(spec))
    ctx.actions.run(executable = ctx.executable._pack, arguments = [manifest.path], inputs = inputs, outputs = [output, checksum], mnemonic = "NpmPackage")
    return [DefaultInfo(files = depset([output, checksum]))]

npm_package = rule(
    implementation = _npm_package_impl,
    attrs = {
        "manifest": attr.label(allow_single_file = True, mandatory = True),
        "srcs": attr.label_list(allow_files = True),
        "engine_manifest": attr.label(allow_single_file = True),
        "ignore_file": attr.label(allow_single_file = True),
        "platform": attr.string(),
        "binding": attr.label(allow_single_file = True),
        "monty": attr.label(executable = True, cfg = "target"),
        "monty_license": attr.label(default = Label("//:licenses/monty-MIT.txt"), allow_single_file = True),
        "_pack": attr.label(default = Label("//build/bazel:npm_package"), executable = True, cfg = "exec"),
    },
)
