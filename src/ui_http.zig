//! Static assets stay in executable read-only storage. Gzip is produced at build
//! time; only clients requesting identity pay for bounded decompression.
const std = @import("std");
const assets = @import("ui_assets");
const Encoding = enum { gzip, identity, unacceptable };

fn encoding(header: ?[]const u8, compressed: bool) Encoding {
    const value = header orelse return .identity;
    var gzip: ?f32 = null;
    var identity: ?f32 = null;
    var wildcard: ?f32 = null;
    var parts = std.mem.splitScalar(u8, value, ',');
    while (parts.next()) |part| {
        var parameters = std.mem.splitScalar(u8, part, ';');
        const name = std.mem.trim(u8, parameters.next().?, " \t");
        var quality: f32 = 1;
        while (parameters.next()) |parameter| {
            const p = std.mem.trim(u8, parameter, " \t");
            if (std.mem.startsWith(u8, p, "q=")) {
                quality = std.fmt.parseFloat(f32, p[2..]) catch 0;
                if (!std.math.isFinite(quality) or quality < 0 or quality > 1) quality = 0;
            }
        }
        if (std.ascii.eqlIgnoreCase(name, "gzip")) gzip = quality;
        if (std.ascii.eqlIgnoreCase(name, "identity")) identity = quality;
        if (std.mem.eql(u8, name, "*")) wildcard = quality;
    }
    const gz = if (compressed) gzip orelse wildcard orelse 0 else 0;
    const plain = identity orelse if (wildcard != null and wildcard.? == 0) @as(f32, 0) else @as(f32, 1);
    if (gz > 0 and gz >= plain) return .gzip;
    if (plain > 0) return .identity;
    if (gz > 0) return .gzip;
    return .unacceptable;
}
fn matches(value: []const u8, etag: []const u8) bool {
    var tags = std.mem.splitScalar(u8, value, ',');
    while (tags.next()) |tag| {
        const trimmed = std.mem.trim(u8, tag, " \t");
        if (std.mem.eql(u8, trimmed, "*")) return true;
        const candidate = if (std.mem.startsWith(u8, trimmed, "W/")) trimmed[2..] else trimmed;
        if (std.mem.eql(u8, candidate, etag[2..])) return true;
    }
    return false;
}
pub fn respond(a: std.mem.Allocator, request: *std.http.Server.Request) !bool {
    if (request.head.method != .GET and request.head.method != .HEAD) return false;
    const path = std.mem.sliceTo(request.head.target, '?');
    const asset = assets.get(if (std.mem.eql(u8, path, "/receive")) "/" else path) orelse return false;
    var accept_encoding: ?[]const u8 = null;
    var if_none_match: ?[]const u8 = null;
    var iterator = request.iterateHeaders();
    while (iterator.next()) |header| {
        if (std.ascii.eqlIgnoreCase(header.name, "accept-encoding")) accept_encoding = header.value;
        if (std.ascii.eqlIgnoreCase(header.name, "if-none-match")) if_none_match = header.value;
    }
    const chosen = encoding(accept_encoding, asset.gzip);
    if (chosen == .unacceptable) {
        try request.respond("No acceptable asset encoding", .{ .status = .not_acceptable, .keep_alive = false, .extra_headers = &.{.{ .name = "Vary", .value = "Accept-Encoding" }} });
        return true;
    }
    var headers: [9]std.http.Header = undefined;
    headers[0..8].* = .{
        .{ .name = "Content-Type", .value = asset.content_type },
        .{ .name = "Cache-Control", .value = if (asset.immutable) "public, max-age=31536000, immutable" else "no-cache" },
        .{ .name = "ETag", .value = asset.etag },
        .{ .name = "Vary", .value = "Accept-Encoding" },
        .{ .name = "X-Content-Type-Options", .value = "nosniff" },
        .{ .name = "Referrer-Policy", .value = "no-referrer" },
        .{ .name = "Content-Security-Policy", .value = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; font-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'" },
        .{ .name = "Cross-Origin-Resource-Policy", .value = "same-origin" },
    };
    const count: usize = if (chosen == .gzip) 9 else 8;
    if (chosen == .gzip) headers[8] = .{ .name = "Content-Encoding", .value = "gzip" };
    if (if_none_match != null and matches(if_none_match.?, asset.etag)) {
        try request.respond("", .{ .status = .not_modified, .keep_alive = false, .extra_headers = headers[0..count], .transfer_encoding = .none });
        return true;
    }
    var body = asset.body;
    if (asset.gzip and chosen == .identity) {
        var reader: std.Io.Reader = .fixed(asset.body);
        var decompressor = std.compress.flate.Decompress.init(&reader, .gzip, &.{});
        body = try decompressor.reader.allocRemaining(a, .limited(asset.raw_size));
        if (body.len != asset.raw_size) return error.InvalidEmbeddedAsset;
    }
    try request.respond(body, .{ .keep_alive = false, .extra_headers = headers[0..count] });
    return true;
}
test "gzip negotiation respects exclusions and quality" {
    const t = std.testing;
    try t.expectEqual(Encoding.identity, encoding(null, true));
    try t.expectEqual(Encoding.gzip, encoding("gzip, deflate, br", true));
    try t.expectEqual(Encoding.identity, encoding("gzip;q=0, *;q=1", true));
    try t.expectEqual(Encoding.gzip, encoding("*;q=1, identity;q=0", true));
    try t.expectEqual(Encoding.unacceptable, encoding("*;q=0", true));
    try t.expectEqual(Encoding.identity, encoding("gzip;q=nan", true));
    try t.expectEqual(Encoding.identity, encoding("gzip", false));
    try t.expectEqual(Encoding.gzip, encoding("gzip;q=0.5, identity;q=0.2", true));
}
test "conditional GET matches weak and strong validators" {
    try std.testing.expect(matches("\"other\", \"hash\"", "W/\"hash\""));
    try std.testing.expect(matches("W/\"hash\"", "W/\"hash\""));
    try std.testing.expect(!matches("\"other\"", "W/\"hash\""));
}
