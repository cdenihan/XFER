const std = @import("std");
const Io = std.Io;
pub const max_path = 4096;
pub const max_depth = 64;

/// Wire paths always use '/', including on Windows.
pub fn validate(path: []const u8) !void {
    if (path.len == 0 or path.len > max_path) return error.InvalidPath;
    var components = std.mem.splitScalar(u8, path, '/');
    var depth: usize = 0;
    while (components.next()) |part| {
        try validateName(part);
        depth += 1;
        if (depth > max_depth) return error.PathTooDeep;
    }
}

pub fn validateName(name: []const u8) !void {
    if (name.len == 0 or name.len > 255 or !std.unicode.utf8ValidateSlice(name)) return error.InvalidName;
    if (name[name.len - 1] == '.' or name[name.len - 1] == ' ') return error.InvalidName;
    for (name) |c| {
        if (c < 32 or c == 127 or std.mem.findScalar(u8, "<>:\"/\\|?*", c) != null) return error.InvalidName;
    }
    var utf8 = (try std.unicode.Utf8View.init(name)).iterator();
    while (utf8.nextCodepoint()) |c| {
        if ((c >= 0x80 and c <= 0x9f) or (c >= 0x202a and c <= 0x202e) or (c >= 0x2066 and c <= 0x2069)) return error.InvalidName;
    }
    const stem = name[0 .. std.mem.findScalar(u8, name, '.') orelse name.len];
    for ([_][]const u8{ "CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$" }) |reserved| {
        if (std.ascii.eqlIgnoreCase(stem, reserved)) return error.InvalidName;
    }
    if (stem.len >= 4 and (std.ascii.eqlIgnoreCase(stem[0..3], "COM") or std.ascii.eqlIgnoreCase(stem[0..3], "LPT"))) {
        const suffix = stem[3..];
        if ((suffix.len == 1 and suffix[0] >= '1' and suffix[0] <= '9') or
            std.mem.eql(u8, suffix, "¹") or std.mem.eql(u8, suffix, "²") or std.mem.eql(u8, suffix, "³")) return error.InvalidName;
    }
}

/// Traverse components separately: no-follow applies to ancestors too.
pub fn openDir(root: Io.Dir, io: Io, path: []const u8) !Io.Dir {
    var dir = try root.openDir(io, ".", .{ .follow_symlinks = false });
    errdefer dir.close(io);
    if (path.len == 0) return dir;
    try validate(path);
    var parts = std.mem.splitScalar(u8, path, '/');
    while (parts.next()) |part| {
        const next = try dir.openDir(io, part, .{ .follow_symlinks = false });
        dir.close(io);
        dir = next;
    }
    return dir;
}

pub const Parent = struct {
    dir: Io.Dir,
    name: []const u8,
    pub fn close(self: Parent, io: Io) void {
        self.dir.close(io);
    }
};
pub fn parent(root: Io.Dir, io: Io, path: []const u8) !Parent {
    try validate(path);
    const split = std.mem.lastIndexOfScalar(u8, path, '/');
    return .{
        .dir = try openDir(root, io, if (split) |i| path[0..i] else ""),
        .name = if (split) |i| path[i + 1 ..] else path,
    };
}
pub fn openFile(root: Io.Dir, io: Io, path: []const u8) !Io.File {
    const p = try parent(root, io, path);
    defer p.close(io);
    const file = try p.dir.openFile(io, p.name, .{ .follow_symlinks = false, .allow_directory = false });
    errdefer file.close(io);
    if ((try file.stat(io)).kind != .file) return error.UnsupportedFile;
    return file;
}

test "portable paths reject traversal, devices, streams, controls and ambiguous endings" {
    for ([_][]const u8{ "", "/abs", "../escape", "a/../b", "a//b", "a\\b", "C:foo", "aux.txt", "LPT1", "COM¹.txt", "x.", "x ", "a/", "a\x1bb", "a\n", "\xff", "a\u{202e}b" }) |path| {
        try std.testing.expect(if (validate(path)) |_| false else |_| true);
    }
    try validate("photos/été 2026.jpg");
    try validate("a/b/file.txt");
}
