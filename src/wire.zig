const std = @import("std");
const Io = std.Io;
const net_io = @import("net_io.zig");
const X25519 = std.crypto.dh.X25519;
const Hkdf = std.crypto.kdf.hkdf.HkdfSha256;
const Sha256 = std.crypto.hash.sha2.Sha256;
const Aead = std.crypto.aead.chacha_poly.ChaCha20Poly1305;
pub const max_payload = 64 * 1024;
const magic = "XFERZ017";
const hello_len = 73;
pub const Kind = enum(u8) { offer = 1, manifest = 2, accept = 3, reject = 4, data = 5, finish = 6, delivered = 7 };
pub const Role = enum { sender, receiver };
pub fn timeout(seconds: i64) Io.Timeout {
    return .{ .duration = .{ .raw = .fromSeconds(seconds), .clock = .awake } };
}

/// A single operation has a finite deadline, even if the peer dribbles bytes.
fn operationDeadline(io: Io, seconds: i64, limit: ?Io.Clock.Timestamp) Io.Timeout {
    const local = timeout(seconds).toTimestamp(io).?;
    return .{ .deadline = if (limit) |end| (if (end.raw.nanoseconds < local.raw.nanoseconds) end else local) else local };
}
fn readExact(stream: Io.net.Stream, io: Io, output: []u8, seconds: i64, limit: ?Io.Clock.Timestamp) !void {
    const deadline = operationDeadline(io, seconds, limit);
    var offset: usize = 0;
    while (offset < output.len) {
        var data = [_][]u8{output[offset..]};
        const result = try (try net_io.operateTimeout(io, .{ .net_read = .{ .socket_handle = stream.socket.handle, .data = &data } }, deadline)).net_read;
        if (result.data_len == 0) return error.EndOfStream;
        offset += result.data_len;
    }
}
fn writeAll(stream: Io.net.Stream, io: Io, input: []const u8, limit: ?Io.Clock.Timestamp) !void {
    const deadline = operationDeadline(io, 300, limit);
    var offset: usize = 0;
    while (offset < input.len) {
        const data = [_][]const u8{input[offset..]};
        const n = try (try net_io.operateTimeout(io, .{ .net_write = .{ .socket_handle = stream.socket.handle, .data = &data } }, deadline)).net_write;
        if (n == 0) return error.EndOfStream;
        offset += n;
    }
}

pub const Channel = struct {
    stream: Io.net.Stream,
    io: Io,
    tx_key: [32]u8,
    rx_key: [32]u8,
    tx_seq: u64 = 0,
    rx_seq: u64 = 0,
    code: [12]u8,
    phase_deadline: Io.Clock.Timestamp,
    tx_buffer: [4 + max_payload + 1 + 16]u8 = undefined,
    rx_buffer: [max_payload + 1 + 16]u8 = undefined,

    pub fn init(stream: Io.net.Stream, io: Io, role: Role, token: []const u8) !Channel {
        var seed: [32]u8 = undefined;
        try io.randomSecure(&seed);
        defer std.crypto.secureZero(u8, &seed);
        var pair = X25519.KeyPair.generateDeterministic(seed);
        defer std.crypto.secureZero(u8, &pair.secret_key);
        var local: [hello_len]u8 = undefined;
        @memcpy(local[0..8], magic);
        @memcpy(local[8..40], &pair.public_key);
        try io.randomSecure(local[40..72]);
        local[72] = @intFromBool(token.len != 0);
        var remote: [hello_len]u8 = undefined;
        // Commit before revealing keys and nonces. This prevents an active
        // intermediary choosing either transcript to grind a matching code.
        var commitment: [32]u8 = undefined;
        Sha256.hash(&local, &commitment, .{});
        var remote_commitment: [32]u8 = undefined;
        if (role == .sender) {
            try writeAll(stream, io, &commitment, null);
            try readExact(stream, io, &remote_commitment, 15, null);
        } else {
            try readExact(stream, io, &remote_commitment, 15, null);
            try writeAll(stream, io, &commitment, null);
        }
        // Ordered reveal bounds receiver exposure to unknown clients.
        if (role == .sender) {
            try writeAll(stream, io, &local, null);
            try readExact(stream, io, &remote, 15, null);
        } else {
            try readExact(stream, io, &remote, 15, null);
            try writeAll(stream, io, &local, null);
        }
        Sha256.hash(&remote, &commitment, .{});
        if (!std.crypto.timing_safe.eql([32]u8, commitment, remote_commitment)) return error.InvalidCommitment;
        if (!std.mem.eql(u8, remote[0..8], magic)) return error.IncompatibleProtocol;
        if (remote[72] != local[72]) return error.TokenRequiredOnBothDevices;
        var shared = try X25519.scalarmult(pair.secret_key, remote[8..40].*);
        defer std.crypto.secureZero(u8, &shared);
        var digest = Sha256.init(.{});
        digest.update("XFER session v1\x00");
        digest.update(if (role == .sender) &local else &remote);
        digest.update(if (role == .sender) &remote else &local);
        digest.update(token);
        const salt = digest.finalResult();
        var prk = Hkdf.extract(&salt, &shared);
        defer std.crypto.secureZero(u8, &prk);
        var keys: [96]u8 = undefined;
        defer std.crypto.secureZero(u8, &keys);
        Hkdf.expand(&keys, "XFER directional keys and comparison code v1", prk);
        const hex = std.fmt.bytesToHex(keys[64..70], .lower);
        return .{
            .stream = stream,
            .io = io,
            .tx_key = if (role == .sender) keys[0..32].* else keys[32..64].*,
            .rx_key = if (role == .sender) keys[32..64].* else keys[0..32].*,
            .code = hex,
            .phase_deadline = timeout(120).toTimestamp(io).?,
        };
    }
    pub fn deinit(self: *Channel) void {
        std.crypto.secureZero(u8, &self.tx_key);
        std.crypto.secureZero(u8, &self.rx_key);
        std.crypto.secureZero(u8, &self.tx_buffer);
        std.crypto.secureZero(u8, &self.rx_buffer);
    }
    pub fn setPhaseTimeout(self: *Channel, seconds: i64) void {
        self.phase_deadline = timeout(seconds).toTimestamp(self.io).?;
    }
    fn checkDeadline(self: *Channel) !void {
        if (self.phase_deadline.durationFromNow(self.io).raw.nanoseconds <= 0) return error.Timeout;
    }
    pub fn send(self: *Channel, kind: Kind, payload: []const u8) !void {
        try self.checkDeadline();
        if (payload.len > max_payload or self.tx_seq == std.math.maxInt(u64)) return error.RecordLimit;
        // Exact overlap is supported by Zig's ChaCha implementation. Encrypt
        // directly in the frame buffer, avoiding a copy and full-buffer wipe
        // for every record (including empty control records and tiny files).
        const ciphertext = self.tx_buffer[4..][0 .. payload.len + 1];
        ciphertext[0] = @backingInt(kind);
        @memcpy(ciphertext[1..], payload);
        var header: [4]u8 = undefined;
        std.mem.writeInt(u32, &header, @intCast(payload.len + 1 + 16), .little);
        var aad: [12]u8 = undefined;
        @memcpy(aad[0..4], &header);
        std.mem.writeInt(u64, aad[4..12], self.tx_seq, .little);
        const nonce = recordNonce(self.tx_seq);
        var tag: [16]u8 = undefined;
        Aead.encrypt(ciphertext, &tag, ciphertext, &aad, nonce, self.tx_key);
        @memcpy(self.tx_buffer[0..4], &header);
        @memcpy(self.tx_buffer[4 + ciphertext.len ..][0..16], &tag);
        try writeAll(self.stream, self.io, self.tx_buffer[0 .. 4 + ciphertext.len + 16], self.phase_deadline);
        self.tx_seq += 1;
    }
    pub const Record = struct { kind: Kind, data: []const u8 };
    /// Borrowed until the next receive. Long consent waits are still bounded.
    pub fn receive(self: *Channel) !Record {
        try self.checkDeadline();
        if (self.rx_seq == std.math.maxInt(u64)) return error.RecordLimit;
        var header: [4]u8 = undefined;
        try readExact(self.stream, self.io, &header, 300, self.phase_deadline);
        const len = std.mem.readInt(u32, &header, .little);
        if (len < 17 or len > max_payload + 17) return error.RecordLimit;
        try readExact(self.stream, self.io, self.rx_buffer[0..len], 60, self.phase_deadline);
        var aad: [12]u8 = undefined;
        @memcpy(aad[0..4], &header);
        std.mem.writeInt(u64, aad[4..12], self.rx_seq, .little);
        const plain_len = len - 16;
        const plaintext = self.rx_buffer[0..plain_len];
        try Aead.decrypt(plaintext, plaintext, self.rx_buffer[plain_len..][0..16].*, &aad, recordNonce(self.rx_seq), self.rx_key);
        self.rx_seq += 1;
        const kind = std.enums.fromInt(Kind, plaintext[0]) orelse return error.InvalidRecord;
        return .{ .kind = kind, .data = plaintext[1..] };
    }
    pub fn expect(self: *Channel, kind: Kind) ![]const u8 {
        const record = try self.receive();
        if (record.kind == .reject) return error.Declined;
        if (record.kind != kind) return error.UnexpectedRecord;
        return record.data;
    }
};
fn recordNonce(seq: u64) [12]u8 {
    var nonce: [12]u8 = @splat(0);
    std.mem.writeInt(u64, nonce[4..12], seq, .little);
    return nonce;
}

test "authenticated records bind length, direction and sequence" {
    const key: [32]u8 = @splat(7);
    var ciphertext: [4]u8 = undefined;
    var tag: [16]u8 = undefined;
    var plain: [4]u8 = undefined;
    const aad = "record";
    Aead.encrypt(&ciphertext, &tag, "test", aad, recordNonce(0), key);
    try Aead.decrypt(&plain, &ciphertext, tag, aad, recordNonce(0), key);
    try std.testing.expectEqualStrings("test", &plain);
    try std.testing.expectError(error.AuthenticationFailed, Aead.decrypt(&plain, &ciphertext, tag, aad, recordNonce(1), key));
    ciphertext[0] ^= 1;
    try std.testing.expectError(error.AuthenticationFailed, Aead.decrypt(&plain, &ciphertext, tag, aad, recordNonce(0), key));
}

test "in-place record crypto matches disjoint buffers at block boundaries" {
    const key: [32]u8 = @splat(7);
    const aad = "record";
    var original: [max_payload + 1]u8 = undefined;
    for (&original, 0..) |*byte, i| byte.* = @truncate(i);
    var inplace: [max_payload + 1]u8 = undefined;
    var separate: [max_payload + 1]u8 = undefined;
    for ([_]usize{ 1, 15, 16, 17, 63, 64, 65, 255, 256, 257, max_payload + 1 }) |len| {
        @memcpy(inplace[0..len], original[0..len]);
        var tag: [16]u8 = undefined;
        var reference_tag: [16]u8 = undefined;
        Aead.encrypt(separate[0..len], &reference_tag, original[0..len], aad, recordNonce(9), key);
        Aead.encrypt(inplace[0..len], &tag, inplace[0..len], aad, recordNonce(9), key);
        try std.testing.expectEqualSlices(u8, separate[0..len], inplace[0..len]);
        try std.testing.expectEqualSlices(u8, &reference_tag, &tag);
        try Aead.decrypt(inplace[0..len], inplace[0..len], tag, aad, recordNonce(9), key);
        try std.testing.expectEqualSlices(u8, original[0..len], inplace[0..len]);
    }
}
