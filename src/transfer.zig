const std = @import("std");
const Io = std.Io;
const paths = @import("paths.zig");
const manifest = @import("manifest.zig");
const wire = @import("wire.zig");
const Reporter = @import("reporter.zig").Reporter;
const confirm = @import("reporter.zig").confirm;

pub const Options = struct {
    token: []const u8 = "",
    yes: bool = false,
    max_bytes: u64 = 16 * 1024 * 1024 * 1024,
    reporter: Reporter,
    approval: ?Approval = null,
    pub const Approval = struct {
        context: *anyopaque,
        ask: *const fn (*anyopaque, [12]u8, manifest.Offer, bool) anyerror!bool,
    };
};
pub fn connect(io: Io, host: []const u8, port: u16) !Io.net.Stream {
    // Zig 0.17's Threaded backend does not implement ConnectOptions.timeout.
    // Race a cancellable connect against a timer, closing any losing stream.
    const Result = union(enum) { connected: anyerror!Io.net.Stream, expired: Io.Cancelable!void };
    var results: [2]Result = undefined;
    var select = Io.Select(Result).init(io, &results);
    defer while (select.cancel()) |pending| {
        switch (pending) {
            .connected => |result| {
                if (result) |stream| stream.close(io) else |_| {}
            },
            .expired => {},
        }
    };
    try select.concurrent(.connected, connectInner, .{ io, host, port });
    try select.concurrent(.expired, Io.sleep, .{ io, Io.Duration.fromSeconds(10), Io.Clock.awake });
    return switch (try select.await()) {
        .connected => |result| result,
        .expired => error.Timeout,
    };
}
fn connectInner(io: Io, host: []const u8, port: u16) !Io.net.Stream {
    if (host.len > 0 and host[0] == '[') {
        const end = std.mem.findScalar(u8, host, ']') orelse return error.InvalidAddress;
        const suffix = host[end + 1 ..];
        const endpoint_port = if (suffix.len == 0) port else blk: {
            if (suffix[0] != ':') return error.InvalidAddress;
            const parsed = std.fmt.parseInt(u16, suffix[1..], 10) catch return error.InvalidPort;
            if (parsed == 0) return error.InvalidPort;
            break :blk parsed;
        };
        // resolve (rather than parse) supports link-local interface scopes.
        const address = try Io.net.IpAddress.resolve(io, host[1..end], endpoint_port);
        return address.connect(io, .{ .mode = .stream, .protocol = .tcp });
    }
    if (Io.net.IpAddress.resolve(io, host, port)) |address| return address.connect(io, .{ .mode = .stream, .protocol = .tcp }) else |_| {}
    if (Io.net.IpAddress.parseLiteral(host)) |parsed| {
        var address = parsed;
        if (address.getPort() == 0) address.setPort(port);
        return address.connect(io, .{ .mode = .stream, .protocol = .tcp });
    } else |_| {}
    return (try Io.net.HostName.init(host)).connect(io, port, .{ .mode = .stream, .protocol = .tcp });
}

pub fn send(a: std.mem.Allocator, io: Io, stream: Io.net.Stream, plan: manifest.Plan, options: Options) !void {
    var channel = try wire.Channel.init(stream, io, .sender, options.token);
    defer channel.deinit();
    const encoded = try std.json.Stringify.valueAlloc(a, plan.offer, .{});
    defer a.free(encoded);
    if (encoded.len > manifest.max_encoded) return error.ManifestTooLarge;
    var digest: [32]u8 = undefined;
    manifest.Sha256.hash(encoded, &digest, .{});
    var size: [4]u8 = undefined;
    std.mem.writeInt(u32, &size, @intCast(encoded.len), .little);
    try channel.send(.offer, &size);
    var offset: usize = 0;
    while (offset < encoded.len) {
        const n = @min(encoded.len - offset, wire.max_payload);
        try channel.send(.manifest, encoded[offset..][0..n]);
        offset += n;
    }
    try consent(&channel, options, plan.offer, false);
    var buffer: [wire.max_payload]u8 = undefined;
    var sent: u64 = 0;
    var reported: u64 = 0;
    for (plan.offer.entries) |entry| {
        if (entry.kind == .directory) continue;
        const file = try paths.openFile(plan.source, io, entry.path);
        defer file.close(io);
        if ((try file.stat(io)).size != entry.size) return error.SourceChanged;
        var hash = manifest.Sha256.init(.{});
        var position: u64 = 0;
        while (position < entry.size) {
            const chunk = buffer[0..@min(buffer.len, entry.size - position)];
            if (try file.readPositionalAll(io, chunk, position) != chunk.len) return error.SourceChanged;
            hash.update(chunk);
            try channel.send(.data, chunk);
            position += chunk.len;
            sent += chunk.len;
            if (sent - reported >= 4 * 1024 * 1024) {
                try options.reporter.event("progress", entry.path, sent, plan.offer.total);
                reported = sent;
            }
        }
        if (!std.mem.eql(u8, &hash.finalResult(), &entry.hash) or (try file.stat(io)).size != entry.size) return error.SourceChanged;
    }
    try channel.send(.finish, &digest);
    const delivered = try channel.expect(.delivered);
    if (delivered.len == 0 or delivered.len > 255) return error.InvalidRecord;
    try paths.validateName(delivered);
    try options.reporter.event("sent", delivered, sent, plan.offer.total);
}

pub fn receive(a: std.mem.Allocator, io: Io, stream: Io.net.Stream, output: Io.Dir, options: Options) !void {
    var channel = try wire.Channel.init(stream, io, .receiver, options.token);
    defer channel.deinit();
    const offer_header = try channel.expect(.offer);
    if (offer_header.len != 4) return error.InvalidManifest;
    const size = std.mem.readInt(u32, offer_header[0..4], .little);
    if (size == 0 or size > manifest.max_encoded) return error.ManifestTooLarge;
    const encoded = try a.alloc(u8, size);
    defer a.free(encoded);
    var offset: usize = 0;
    while (offset < size) {
        const record = try channel.expect(.manifest);
        if (record.len == 0 or record.len > size - offset) return error.InvalidManifest;
        @memcpy(encoded[offset..][0..record.len], record);
        offset += record.len;
    }
    var digest: [32]u8 = undefined;
    manifest.Sha256.hash(encoded, &digest, .{});
    const parsed = try std.json.parseFromSlice(manifest.Offer, a, encoded, .{ .allocate = .alloc_always, .max_value_len = paths.max_path });
    defer parsed.deinit();
    const offer = parsed.value;
    try manifest.validate(a, offer, options.max_bytes);
    try consent(&channel, options, offer, true);
    // Staging is a fresh private directory on the destination filesystem.
    // All creates are exclusive and publication never replaces an existing item.
    var random: [16]u8 = undefined;
    try io.randomSecure(&random);
    const stage_name = try std.fmt.allocPrint(a, ".xfer-{s}.part", .{std.fmt.bytesToHex(random, .lower)});
    defer a.free(stage_name);
    try output.createDir(io, stage_name, privateDirPermissions());
    defer output.deleteTree(io, stage_name) catch {};
    const stage = try output.openDir(io, stage_name, .{ .follow_symlinks = false });
    defer stage.close(io);
    var received: u64 = 0;
    var reported: u64 = 0;
    for (offer.entries) |entry| {
        const p = try paths.parent(stage, io, entry.path);
        defer p.close(io);
        if (entry.kind == .directory) {
            try p.dir.createDir(io, p.name, privateDirPermissions());
            continue;
        }
        const file = try p.dir.createFile(io, p.name, .{ .exclusive = true, .permissions = privateFilePermissions() });
        defer file.close(io);
        var hash = manifest.Sha256.init(.{});
        var position: u64 = 0;
        while (position < entry.size) {
            const chunk = try channel.expect(.data);
            if (chunk.len == 0 or chunk.len > entry.size - position) return error.InvalidRecord;
            hash.update(chunk);
            try file.writePositionalAll(io, chunk, position);
            position += chunk.len;
            received += chunk.len;
            if (received - reported >= 4 * 1024 * 1024) {
                try options.reporter.event("progress", entry.path, received, offer.total);
                reported = received;
            }
        }
        if (!std.mem.eql(u8, &hash.finalResult(), &entry.hash)) return error.IntegrityMismatch;
        try file.sync(io);
    }
    const finished = try channel.expect(.finish);
    if (!std.mem.eql(u8, finished, &digest) or received != offer.total) return error.IntegrityMismatch;
    const final_name = try publish(a, io, stage, output, offer.name);
    defer a.free(final_name);
    // Once published, delivery is committed even if the acknowledgement is lost.
    try options.reporter.event("received", final_name, received, offer.total);
    try channel.send(.delivered, final_name);
}

fn consent(channel: *wire.Channel, options: Options, offer: manifest.Offer, receiving: bool) !void {
    channel.setPhaseTimeout(300);
    if (options.yes and options.token.len < 16) return error.SharedSecretRequired;
    var buffer: [1024]u8 = undefined;
    const summary = try std.fmt.bufPrint(&buffer, "{s} {s}: {d} entries, {d} bytes. Compare code {s}-{s}-{s} on both devices.", .{
        if (receiving) "Receive" else "Send", offer.name,         offer.entries.len,   offer.total,
        channel.code[0..4],                   channel.code[4..8], channel.code[8..12],
    });
    try options.reporter.event("offer", summary, 0, offer.total);
    const approved = if (options.approval) |approval| try approval.ask(approval.context, channel.code, offer, receiving) else options.yes or (!options.reporter.json and (try confirm(channel.io, "Codes match and transfer approved? [y/N] ")));
    if (!approved) {
        try channel.send(.reject, "");
        return error.Declined;
    }
    try channel.send(.accept, "");
    if ((try channel.expect(.accept)).len != 0) return error.InvalidRecord;
    channel.setPhaseTimeout(24 * 60 * 60);
    try options.reporter.event("accepted", "Both devices approved. Transferring…", 0, offer.total);
}

fn publish(a: std.mem.Allocator, io: Io, stage: Io.Dir, output: Io.Dir, name: []const u8) ![]u8 {
    var number: usize = 0;
    while (number < 10_000) : (number += 1) {
        const candidate = if (number == 0) try a.dupe(u8, name) else try collisionName(a, name, number);
        stage.renamePreserve(name, output, candidate, io) catch |err| {
            a.free(candidate);
            if (err == error.PathAlreadyExists) continue;
            return err;
        };
        return candidate;
    }
    return error.TooManyCollisions;
}
fn collisionName(a: std.mem.Allocator, name: []const u8, number: usize) ![]u8 {
    // Long UTF-8 names are shortened at a codepoint boundary to make room.
    const suffix = try std.fmt.allocPrint(a, " ({d})", .{number});
    defer a.free(suffix);
    var end = @min(name.len, 255 - suffix.len);
    while (end > 0 and !std.unicode.utf8ValidateSlice(name[0..end])) end -= 1;
    return std.fmt.allocPrint(a, "{s}{s}", .{ name[0..end], suffix });
}
fn privateDirPermissions() Io.File.Permissions {
    if (@import("builtin").os.tag == .windows) return .default_dir;
    return .fromMode(0o700);
}
fn privateFilePermissions() Io.File.Permissions {
    if (@import("builtin").os.tag == .windows) return .default_file;
    return .fromMode(0o600);
}

test "collision suffixes stay within portable UTF-8 limits" {
    const a = std.testing.allocator;
    var name: [254]u8 = undefined;
    for (0..127) |i| {
        name[i * 2] = 0xc3;
        name[i * 2 + 1] = 0xa9;
    }
    const result = try collisionName(a, &name, 123);
    defer a.free(result);
    try paths.validateName(result);
    try std.testing.expect(result.len <= 255);
}
