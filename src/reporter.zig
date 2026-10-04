const std = @import("std");
const Io = std.Io;
pub const Reporter = struct {
    io: Io,
    json: bool = false,
    sink: ?Sink = null,
    pub const Sink = struct {
        context: *anyopaque,
        emit: *const fn (*anyopaque, []const u8, []const u8, u64, u64) anyerror!void,
    };
    pub fn event(self: Reporter, name: []const u8, message: []const u8, bytes: u64, total: u64) !void {
        if (self.sink) |sink| return sink.emit(sink.context, name, message, bytes, total);
        if (self.json) {
            var buffer: [4096]u8 = undefined;
            var writer = Io.File.stdout().writer(self.io, &buffer);
            try std.json.Stringify.value(.{ .event = name, .message = message, .bytes = bytes, .total = total }, .{}, &writer.interface);
            try writer.interface.writeByte('\n');
            try writer.interface.flush();
        } else {
            var buffer: [4096]u8 = undefined;
            var writer = Io.File.stderr().writer(self.io, &buffer);
            if (std.mem.eql(u8, name, "progress")) {
                try writer.interface.print("  {d}/{d} bytes\n", .{ bytes, total });
            } else try writer.interface.print("{s}\n", .{message});
            try writer.interface.flush();
        }
    }
};

/// Bounded byte-by-byte input avoids prefetching the next consent answer.
pub fn readLine(io: Io, buffer: []u8) ![]const u8 {
    var len: usize = 0;
    while (true) {
        var byte: [1]u8 = undefined;
        const count = Io.File.stdin().readStreaming(io, &.{&byte}) catch |err| switch (err) {
            error.EndOfStream => return error.InteractiveInputRequired,
            else => return err,
        };
        if (count == 0) return error.InteractiveInputRequired;
        if (byte[0] == '\n') return std.mem.trim(u8, buffer[0..len], " \r\t");
        if (len == buffer.len) return error.InputTooLong;
        buffer[len] = byte[0];
        len += 1;
    }
}
pub fn confirm(io: Io, prompt: []const u8) !bool {
    if (!try Io.File.stdin().isTty(io)) return error.InteractiveInputRequired;
    try Io.File.stderr().writeStreamingAll(io, prompt);
    const Result = union(enum) { answer: anyerror!bool, expired: Io.Cancelable!void };
    var results: [2]Result = undefined;
    var select = Io.Select(Result).init(io, &results);
    defer select.cancelDiscard();
    try select.concurrent(.answer, readAnswer, .{io});
    try select.concurrent(.expired, Io.sleep, .{ io, Io.Duration.fromSeconds(300), Io.Clock.awake });
    return switch (try select.await()) {
        .answer => |answer| answer,
        .expired => error.Timeout,
    };
}
fn readAnswer(io: Io) !bool {
    var buffer: [16]u8 = undefined;
    const answer = try readLine(io, &buffer);
    return std.ascii.eqlIgnoreCase(answer, "y") or std.ascii.eqlIgnoreCase(answer, "yes");
}
