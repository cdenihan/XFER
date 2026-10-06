const std = @import("std");
const xfer = @import("xfer");
pub fn main(init: std.process.Init) void {
    xfer.cli.run(init) catch |err| {
        const args = init.minimal.args.toSlice(init.arena.allocator()) catch &.{};
        var json = false;
        for (args) |arg| if (std.mem.eql(u8, arg, "--json")) {
            json = true;
        };
        const Reporter = @import("xfer").cli.Reporter;
        const reporter: Reporter = .{ .io = init.io, .json = json };
        reporter.event("error", @errorName(err), 0, 0) catch {};
        if (!json) std.debug.print("Run xfer --help for usage.\n", .{});
        std.process.exit(1);
    };
}
