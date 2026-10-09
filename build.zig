const std = @import("std");
pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});
    b.dependOnFileContents(b.path("VERSION"));
    const version_file = std.Io.Dir.cwd().readFileAlloc(b.graph.io, b.root.joinString(b.allocator, "VERSION") catch @panic("Out of memory"), b.allocator, .limited(64)) catch @panic("Cannot read VERSION");
    const version = std.mem.trim(u8, version_file, " \r\n");
    const options = b.addOptions();
    options.addOption([]const u8, "version", version);
    // The cached, target-independent build produces an index plus a compressed
    // binary payload. @embedFile places it directly in the executable.
    const build_web = b.addSystemCommand(&.{ "bun", "web/scripts/build.ts" });
    build_web.setCwd(b.path("."));
    const web_output = build_web.addOutputDirectoryArg("ui");
    for ([_][]const u8{ "web/package.json", "web/bun.lock", "web/index.html", "web/tsconfig.json", "web/vite.config.ts", "web/scripts/build.ts", "web/scripts/embed.ts" }) |path| build_web.addFileInput(b.path(path));
    for ([_][]const u8{ "web/src", "web/public" }) |path| addWebInputs(b, build_web, path);
    const ui_assets = b.createModule(.{
        .root_source_file = web_output.path(b, "assets.zig"),
        .target = target,
        .optimize = optimize,
    });
    b.step("web", "Build and embed the compressed Vite+ frontend using Bun").dependOn(&build_web.step);
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

fn addWebInputs(b: *std.Build, run: *std.Build.Step.Run, path: []const u8) void {
    b.dependOnDirectoryContents(b.path(path));
    var dir = std.Io.Dir.cwd().openDir(b.graph.io, b.root.joinString(b.allocator, path) catch @panic("Out of memory"), .{ .iterate = true }) catch @panic("Cannot open frontend source directory");
    defer dir.close(b.graph.io);
    var walker = dir.walk(b.allocator) catch @panic("Out of memory");
    defer walker.deinit();
    var files: std.ArrayList([]const u8) = .empty;
    while (walker.next(b.graph.io) catch @panic("Cannot enumerate frontend sources")) |entry| {
        const full = b.pathJoin(&.{ path, entry.path });
        if (entry.kind == .directory) b.dependOnDirectoryContents(b.path(full));
        if (entry.kind == .file and !std.mem.endsWith(u8, entry.path, ".test.ts")) files.append(b.allocator, full) catch @panic("Out of memory");
    }
    std.mem.sort([]const u8, files.items, {}, struct {
        fn less(_: void, left: []const u8, right: []const u8) bool {
            return std.mem.lessThan(u8, left, right);
        }
    }.less);
    for (files.items) |file| run.addFileInput(b.path(file));
}
