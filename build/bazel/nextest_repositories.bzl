"""Pinned native nextest runner for supported development and CI hosts."""

def _runner_repository(ctx):
    if ctx.os.name == "mac os x":
        platform = "mac"
        sha256 = "6ce5c844ae3cdac3f6f42fd86bf71a8bf99aecd76bab9f6743c808223d31fad1"
    elif ctx.os.name == "linux" and ctx.os.arch in ["aarch64", "arm64"]:
        platform = "linux-arm-musl"
        sha256 = "2497ddfd2a0c6805f7c5514ff680c6ca9c1ffc5b9ab30af43e85830b768c129a"
    elif ctx.os.name == "linux" and ctx.os.arch in ["amd64", "x86_64"]:
        platform = "linux-musl"
        sha256 = "5e93f3b4244e2f8bffe5818b2a7cbd6a59f32d262dcb6017905e345cc74227af"
    else:
        fail("Unsupported nextest host: %s/%s" % (ctx.os.name, ctx.os.arch))
    ctx.download_and_extract(
        url = "https://get.nexte.st/0.9.132/" + platform,
        sha256 = sha256,
        type = "tar.gz",
    )
    ctx.file("BUILD.bazel", 'exports_files(["cargo-nextest"], visibility = ["//visibility:public"])\n')

_nextest_repository = repository_rule(implementation = _runner_repository)

def _extension_impl(ctx):
    _nextest_repository(name = "nextest_runner")

nextest_tools = module_extension(implementation = _extension_impl)
