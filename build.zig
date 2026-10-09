const std = @import("std");
pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});
    b.dependOnFileContents(b.path("VERSION"));
    const version_file = std.Io.Dir.cwd().readFileAlloc(b.graph.io, b.root.joinString(b.allocator, "VERSION") catch @panic("Out of memory"), b.allocator, .limited(64)) catch @panic("Cannot read VERSION");
    const version = std.mem.trim(u8, version_file, " \r\n");
    const options = b.addOptions();
    options.addOption([]const u8, "version", version);
    // Bun/Vite+ is a build-time dependency only. Embed the generated module's
    // bytes directly so releases need no runtime, helper or extracted assets.
    const install_web = b.addSystemCommand(&.{ "bun", "install", "--frozen-lockfile" });
    install_web.setCwd(b.path("web"));
    const build_web = b.addSystemCommand(&.{ "bun", "--bun", "run", "vp", "build" });
    build_web.setCwd(b.path("web"));
    build_web.step.dependOn(&install_web.step);
    const embed_web = b.addSystemCommand(&.{ "bun", "scripts/embed.ts" });
    embed_web.setCwd(b.path("web"));
    embed_web.step.dependOn(&build_web.step);
    // Vite emits changing hashed filenames; regenerate after every Vite build
    // rather than caching this command using only its fixed arguments.
    embed_web.has_side_effects = true;
    const ui_assets = b.createModule(.{
        .root_source_file = embed_web.addOutputFileArg("ui_assets.zig"),
        .target = target,
        .optimize = optimize,
    });
    b.step("web", "Build and embed the Vite+ frontend using Bun").dependOn(&embed_web.step);
    const core = b.addModule("xfer", .{ .root_source_file = b.path("src/root.zig"), .target = target, .optimize = optimize });
    core.addOptions("build_options", options);
    core.addImport("ui_assets", ui_assets);
    const exe = b.addExecutable(.{
        .name = "xfer",
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/main.zig"),
            .target = target,
            .optimize = optimize,
            .imports = &.{.{ .name = "xfer", .module = core }},
        }),
    });
    b.installArtifact(exe);
    const run = b.addRunArtifact(exe);
    run.addPassthruArgs();
    b.step("run", "Run XFER").dependOn(&run.step);
    const tests = b.addTest(.{ .root_module = core });
    b.step("test", "Run core tests").dependOn(&b.addRunArtifact(tests).step);
    b.step("check", "Compile without running").dependOn(&exe.step);
}
