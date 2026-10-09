//! Optional installed Tailcat transport. Zig's transfer protocol remains end to
//! end; only its TCP listener is exposed, never the browser-control listener.
const std = @import("std");
const Io = std.Io;
pub const Invite = struct { address: []const u8, port: u16 };
pub fn parseInvite(value: []const u8) !Invite {
    if (!std.mem.startsWith(u8, value, "xfer-tailcat:")) return error.InvalidTailcatInvite;
    const remaining = value[13..];
    const colon = std.mem.indexOfScalar(u8, remaining, ':') orelse return error.InvalidTailcatInvite;
    const port = std.fmt.parseInt(u16, remaining[0..colon], 10) catch return error.InvalidTailcatInvite;
    if (port == 0) return error.InvalidTailcatInvite;
    const address = remaining[colon + 1 ..];
    if (address.len < 40 or address.len > 2048 or !std.mem.startsWith(u8, address, "tc")) return error.InvalidTailcatInvite;
    for (address) |byte| if (!(std.ascii.isAlphanumeric(byte) or byte == '-' or byte == '_')) return error.InvalidTailcatInvite;
    return .{ .address = address, .port = port };
}
pub fn available(a: std.mem.Allocator, io: Io, executable: []const u8) bool {
    const result = std.process.run(a, io, .{ .argv = &.{ executable, "version" }, .stdout_limit = .limited(128), .stderr_limit = .limited(128), .timeout = @import("wire.zig").timeout(3) }) catch return false;
    defer a.free(result.stdout);
    defer a.free(result.stderr);
    if (!result.term.success()) return false;
    const text = std.mem.trim(u8, result.stdout, " \r\n");
    const version = std.SemanticVersion.parse(if (std.mem.startsWith(u8, text, "v")) text[1..] else text) catch return false;
    return version.major > 0 or version.minor >= 7;
}
fn readLine(a: std.mem.Allocator, io: Io, file: Io.File) ![]const u8 {
    var buffer: [4096]u8 = undefined;
    var reader = file.reader(io, &buffer);
    const text = try reader.interface.takeDelimiterExclusive('\n');
    return a.dupe(u8, std.mem.trim(u8, text, "\r"));
}
fn line(a: std.mem.Allocator, io: Io, file: Io.File) ![]const u8 {
    const Result = union(enum) { text: anyerror![]const u8, timeout: Io.Cancelable!void };
    var results: [2]Result = undefined;
    var select = Io.Select(Result).init(io, &results);
    defer select.cancelDiscard();
    try select.concurrent(.text, readLine, .{ a, io, file });
    try select.concurrent(.timeout, Io.sleep, .{ io, Io.Duration.fromSeconds(15), .awake });
    return switch (try select.await()) {
        .text => |result| result,
        .timeout => error.TailcatStartupTimeout,
    };
}
pub fn server(a: std.mem.Allocator, io: Io, executable: []const u8, port: u16) !struct { child: std.process.Child, invite: []const u8 } {
    const port_text = try std.fmt.allocPrint(a, "{d}", .{port});
    var child = std.process.spawn(io, .{ .argv = &.{ executable, "--json", "--key=new", "serve", "--full-address", port_text }, .stdin = .ignore, .stdout = .pipe, .stderr = .ignore, .create_no_window = true }) catch return error.TailcatNotInstalled;
    errdefer child.kill(io);
    const startup = line(a, io, child.stdout.?) catch |err| return if (err == error.Canceled) err else error.TailcatStartupFailed;
    const parsed = std.json.parseFromSlice(struct { listenAddr: []const u8 }, a, startup, .{}) catch return error.TailcatStartupFailed;
    defer parsed.deinit();
    const invite = try std.fmt.allocPrint(a, "xfer-tailcat:{d}:{s}", .{ port, parsed.value.listenAddr });
    _ = try parseInvite(invite);
    return .{ .child = child, .invite = invite };
}
pub fn forward(a: std.mem.Allocator, io: Io, executable: []const u8, invitation: []const u8) !struct { child: std.process.Child, endpoint: []const u8 } {
    const invite = try parseInvite(invitation);
    const mapping = try std.fmt.allocPrint(a, "0:{d}", .{invite.port});
    var child = std.process.spawn(io, .{ .argv = &.{ executable, "--key=new", "forward", "--bind=127.0.0.1", invite.address, mapping }, .stdin = .ignore, .stdout = .ignore, .stderr = .pipe, .create_no_window = true }) catch return error.TailcatNotInstalled;
    errdefer child.kill(io);
    const startup = line(a, io, child.stderr.?) catch |err| return if (err == error.Canceled) err else error.TailcatStartupFailed;
    const prefix = "# forwarding 127.0.0.1:";
    if (!std.mem.startsWith(u8, startup, prefix)) return error.TailcatStartupFailed;
    const rest = startup[prefix.len..];
    const space = std.mem.indexOfScalar(u8, rest, ' ') orelse return error.TailcatStartupFailed;
    const port = std.fmt.parseInt(u16, rest[0..space], 10) catch return error.TailcatStartupFailed;
    if (port == 0) return error.TailcatStartupFailed;
    return .{ .child = child, .endpoint = try std.fmt.allocPrint(a, "127.0.0.1:{d}", .{port}) };
}
pub fn waitForExit(io: Io, file: Io.File) !void {
    var buffer: [1024]u8 = undefined;
    var reader = file.reader(io, &buffer);
    while (true) _ = reader.interface.takeDelimiterExclusive('\n') catch |err| {
        if (err == error.EndOfStream) return;
        return err;
    };
}
test "invitations preserve case and reject arbitrary helper arguments" {
    const valid = "xfer-tailcat:9000:tcABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnop";
    const invite = try parseInvite(valid);
    try std.testing.expectEqual(@as(u16, 9000), invite.port);
    try std.testing.expect(std.mem.startsWith(u8, invite.address, "tcABC"));
    try std.testing.expectError(error.InvalidTailcatInvite, parseInvite("--serve=all"));
    try std.testing.expectError(error.InvalidTailcatInvite, parseInvite("xfer-tailcat:0:tcABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnop"));
    try std.testing.expectError(error.InvalidTailcatInvite, parseInvite("xfer-tailcat:9000:tcABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnop;id"));
}
