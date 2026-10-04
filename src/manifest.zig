const std = @import("std");
const Io = std.Io;
const paths = @import("paths.zig");
pub const Sha256 = std.crypto.hash.sha2.Sha256;
pub const max_entries = 100_000;
pub const max_encoded = 16 * 1024 * 1024;
pub const Entry = struct {
    path: []const u8,
    kind: enum { file, directory },
    size: u64 = 0,
    hash: [32]u8 = @splat(0),
};
pub const Offer = struct {
    name: []const u8,
    entries: []const Entry,
    total: u64,
};
pub const Plan = struct {
    source: Io.Dir,
    offer: Offer,
    skipped: usize,
    pub fn close(self: Plan, io: Io) void {
        self.source.close(io);
    }
};

pub fn hashFile(file: Io.File, io: Io, size: u64) ![32]u8 {
    var hash = Sha256.init(.{});
    var buffer: [64 * 1024]u8 = undefined;
    var offset: u64 = 0;
    while (offset < size) {
        const chunk = buffer[0..@min(buffer.len, size - offset)];
        const n = try file.readPositionalAll(io, chunk, offset);
        if (n != chunk.len) return error.SourceChanged;
        hash.update(chunk);
        offset += n;
    }
    if ((try file.stat(io)).size != size) return error.SourceChanged;
    return hash.finalResult();
}

pub fn plan(allocator: std.mem.Allocator, io: Io, input: []const u8) !Plan {
    const cwd = Io.Dir.cwd();
    const stat = try cwd.statFile(io, input, .{ .follow_symlinks = false });
    if (stat.kind != .file and stat.kind != .directory) return error.UnsupportedFile;
    const real = try cwd.realPathFileAlloc(io, input, allocator);
    const name = try allocator.dupe(u8, std.fs.path.basename(real));
    try paths.validateName(name);
    const source = try cwd.openDir(io, std.fs.path.dirname(real) orelse return error.InvalidPath, .{});
    errdefer source.close(io);
    var entries: std.ArrayList(Entry) = .empty;
    var skipped: usize = 0;
    var path_bytes: usize = 0;
    try scan(allocator, io, source, name, &entries, &skipped, &path_bytes, 0);
    std.mem.sort(Entry, entries.items, {}, lessThan);
    var total: u64 = 0;
    for (entries.items) |entry| total = std.math.add(u64, total, entry.size) catch return error.TransferTooLarge;
    const offer: Offer = .{ .name = name, .entries = entries.items, .total = total };
    try validate(allocator, offer, std.math.maxInt(u64));
    return .{ .source = source, .offer = offer, .skipped = skipped };
}
fn lessThan(_: void, a: Entry, b: Entry) bool {
    return std.mem.order(u8, a.path, b.path) == .lt;
}

fn scan(a: std.mem.Allocator, io: Io, dir: Io.Dir, path: []const u8, entries: *std.ArrayList(Entry), skipped: *usize, path_bytes: *usize, depth: usize) anyerror!void {
    if (depth > paths.max_depth) return error.PathTooDeep;
    try paths.validate(path);
    const p = try paths.parent(dir, io, path);
    defer p.close(io);
    const stat = try p.dir.statFile(io, p.name, .{ .follow_symlinks = false });
    if (stat.kind != .directory and stat.kind != .file) {
        skipped.* += 1;
        return;
    }
    if (entries.items.len >= max_entries) return error.TooManyEntries;
    path_bytes.* += path.len;
    if (path_bytes.* > max_encoded / 2) return error.ManifestTooLarge;
    var entry: Entry = .{ .path = try a.dupe(u8, path), .kind = if (stat.kind == .file) .file else .directory };
    if (stat.kind == .file) {
        const file = try paths.openFile(dir, io, path);
        defer file.close(io);
        entry.size = (try file.stat(io)).size;
        entry.hash = try hashFile(file, io, entry.size);
        try entries.append(a, entry);
    } else {
        try entries.append(a, entry);
        const child = try p.dir.openDir(io, p.name, .{ .follow_symlinks = false, .iterate = true });
        defer child.close(io);
        var it = child.iterate();
        while (try it.next(io)) |item| {
            if (item.kind != .file and item.kind != .directory) {
                skipped.* += 1;
                continue;
            }
            // Iterator names are borrowed. Copy before recursing.
            const next_path = try std.fmt.allocPrint(a, "{s}/{s}", .{ path, item.name });
            try scan(a, io, dir, next_path, entries, skipped, path_bytes, depth + 1);
        }
    }
}

/// Enforce one root, unique portable paths, parent-before-child order, exact
/// totals and finite metadata before asking the user or touching the disk.
pub fn validate(a: std.mem.Allocator, offer: Offer, max_bytes: u64) !void {
    try paths.validateName(offer.name);
    if (offer.entries.len == 0 or offer.entries.len > max_entries) return error.InvalidManifest;
    if (!std.mem.eql(u8, offer.entries[0].path, offer.name)) return error.InvalidManifest;
    var seen: std.StringHashMap(@FieldType(Entry, "kind")) = .init(a);
    defer seen.deinit();
    defer {
        var keys = seen.keyIterator();
        while (keys.next()) |key| a.free(key.*);
    }
    var total: u64 = 0;
    var metadata: usize = 0;
    var previous: ?[]const u8 = null;
    for (offer.entries, 0..) |entry, i| {
        try paths.validate(entry.path);
        metadata += entry.path.len;
        if (metadata > max_encoded / 2) return error.ManifestTooLarge;
        if (previous) |p| if (std.mem.order(u8, p, entry.path) != .lt) return error.InvalidManifest;
        previous = entry.path;
        if (i > 0) {
            if (entry.path.len <= offer.name.len or !std.mem.startsWith(u8, entry.path, offer.name) or entry.path[offer.name.len] != '/') return error.InvalidManifest;
            const slash = std.mem.lastIndexOfScalar(u8, entry.path, '/') orelse return error.InvalidManifest;
            const parent_key = try fold(a, entry.path[0..slash]);
            defer a.free(parent_key);
            if (seen.get(parent_key) != .directory) return error.InvalidManifest;
        }
        const key = try fold(a, entry.path);
        // Map owns keys until validation completes.
        const result = seen.getOrPut(key) catch |err| {
            a.free(key);
            return err;
        };
        if (result.found_existing) {
            a.free(key);
            return error.PathCollision;
        }
        result.value_ptr.* = entry.kind;
        if (entry.kind == .directory) {
            if (entry.size != 0 or !std.mem.allEqual(u8, &entry.hash, 0)) return error.InvalidManifest;
        } else {
            total = std.math.add(u64, total, entry.size) catch return error.TransferTooLarge;
            if (total > max_bytes) return error.TransferTooLarge;
        }
    }
    if (total != offer.total) return error.InvalidManifest;
}
fn fold(a: std.mem.Allocator, path: []const u8) ![]u8 {
    const key = try a.dupe(u8, path);
    for (key) |*c| c.* = std.ascii.toLower(c.*);
    return key;
}

test "manifest rejects escaping roots, missing parents, case aliases, wrong totals and limits" {
    var arena: std.heap.ArenaAllocator = .init(std.testing.allocator);
    defer arena.deinit();
    const a = arena.allocator();
    const root: Entry = .{ .path = "photos", .kind = .directory };
    const file: Entry = .{ .path = "photos/a.jpg", .kind = .file, .size = 4 };
    try validate(a, .{ .name = "photos", .entries = &.{ root, file }, .total = 4 }, 4);
    try std.testing.expectError(error.TransferTooLarge, validate(a, .{ .name = "photos", .entries = &.{ root, file }, .total = 4 }, 3));
    try std.testing.expectError(error.InvalidManifest, validate(a, .{ .name = "photos", .entries = &.{ root, file }, .total = 3 }, 10));
    const bad: Entry = .{ .path = "photos/sub/a", .kind = .file };
    try std.testing.expectError(error.InvalidManifest, validate(a, .{ .name = "photos", .entries = &.{ root, bad }, .total = 0 }, 10));
    const upper: Entry = .{ .path = "photos/A.jpg", .kind = .file, .size = 4 };
    try std.testing.expectError(error.PathCollision, validate(a, .{ .name = "photos", .entries = &.{ root, upper, file }, .total = 8 }, 10));
}
