"""Network-free catalog packaging from declared embedding vectors."""

def _catalog_bundle_impl(ctx):
    output = ctx.actions.declare_directory(ctx.label.name)
    workspace = ctx.file.workspace.dirname or "."
    cache = ctx.file.cache_root.dirname
    ctx.actions.run_shell(
        inputs = depset(ctx.files.sources + ctx.files.embeddings + [ctx.file.workspace, ctx.file.cache_root, ctx.file.package_list, ctx.file.bindings]),
        tools = [ctx.attr.packer[DefaultInfo].files_to_run],
        outputs = [output],
        arguments = [ctx.executable.packer.path, workspace, output.path, cache, ctx.file.package_list.path, ctx.file.bindings.path],
        command = 'set -e; "$1" --workspace "$PWD/$2" --apis-root "$PWD/$2/apis" --output-dir "$PWD/$3" --embedding-cache-dir "$PWD/$4" --package-list "$PWD/$5"; cp "$6" "$3/deployment-bindings.json"',
        mnemonic = "PackCatalogs",
        progress_message = "Packing catalogs from verified cached embeddings",
    )
    return [DefaultInfo(files = depset([output]))]

catalog_bundle = rule(
    implementation = _catalog_bundle_impl,
    attrs = {
        "workspace": attr.label(default = Label("//:Cargo.toml"), allow_single_file = True),
        "sources": attr.label(default = Label("//:catalog_sources")),
        "embeddings": attr.label(default = Label("//:embedding_inputs")),
        "cache_root": attr.label(default = Label("//:build/catalog-embeddings/.gitignore"), allow_single_file = True),
        "package_list": attr.label(allow_single_file = True, mandatory = True),
        "bindings": attr.label(default = Label("//:apis/proof/deployment-bindings.json"), allow_single_file = True),
        "packer": attr.label(default = Label("//crates/plasm-catalog:plasm-pack-catalogs"), executable = True, cfg = "exec"),
    },
)
