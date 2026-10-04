const std = @import("std");
pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});
    b.dependOnFileContents(b.path("VERSION"));
    const version_file = std.Io.Dir.cwd().readFileAlloc(b.graph.io, b.root.joinString(b.allocator, "VERSION") catch @panic("Out of memory"), b.allocator, .limited(64)) catch @panic("Cannot read VERSION");
    const version = std.mem.trim(u8, version_file, " \r\n");
    const options = b.addOptions();
    options.addOption([]const u8, "version", version);
    const core = b.addModule("xfer", .{ .root_source_file = b.path("src/root.zig"), .target = target, .optimize = optimize });
    core.addOptions("build_options", options);
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
