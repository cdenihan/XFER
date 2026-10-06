const std = @import("std");
const Io = std.Io;
const net_io = @import("net_io.zig");
const paths = @import("paths.zig");
const wire = @import("wire.zig");
const query_magic = "XFERQ002";
const reply_magic = "XFERR002";
const linux = @import("builtin").os.tag == .linux;
pub const Peer = struct { name: []const u8, address: Io.net.IpAddress, instance: [16]u8 };
pub const max_peers = 64;

pub fn bind(io: Io, address: Io.net.IpAddress) !Io.net.Socket {
    const socket = try listenAddress(address).bind(io, .{ .mode = .dgram, .protocol = .udp });
    errdefer socket.close(io);
    if (linux and !address.eql(&socket.address)) {
        // Linux needs wildcard binding to receive broadcasts. Require kernel
        // destination-interface metadata before allowing that broader binding.
        const enabled: c_int = 1;
        try std.posix.setsockopt(socket.handle, std.os.linux.IPPROTO.IP, std.os.linux.IP.PKTINFO, std.mem.asBytes(&enabled));
    }
    return socket;
}

fn listenAddress(address: Io.net.IpAddress) Io.net.IpAddress {
    // Other backends retain the exact bind restriction. On Linux every
    // wildcard-received query is filtered by its local interface address.
    return if (linux and address == .ip4 and address.ip4.bytes[0] != 127)
        .{ .ip4 = .unspecified(address.getPort()) }
    else
        address;
}

/// Respond only to discovery queries. No scanning, passive permanent
/// advertisements, remote address supplied in packets, or file metadata.
pub fn serve(io: Io, socket: Io.net.Socket, address: Io.net.IpAddress, port: u16, name: []const u8, instance: [16]u8) void {
    serveInner(io, socket, address, port, name, instance) catch |err| {
        if (err != error.Canceled) std.log.warn("Nearby discovery stopped: {s}", .{@errorName(err)});
    };
}
fn serveInner(io: Io, socket: Io.net.Socket, address: Io.net.IpAddress, port: u16, name: []const u8, instance: [16]u8) !void {
    var response_socket: ?Io.net.Socket = null;
    defer if (response_socket) |bound| bound.close(io);
    if (!address.eql(&socket.address)) {
        var source = address;
        source.setPort(0);
        response_socket = try source.bind(io, .{ .mode = .dgram, .protocol = .udp });
    }
    const responder = response_socket orelse socket;
    var input: [256]u8 = undefined;
    var reply: [106]u8 = undefined;
    @memcpy(reply[0..8], reply_magic);
    std.mem.writeInt(u16, reply[24..26], port, .little);
    reply[26] = @intCast(name.len);
    @memcpy(reply[27..43], &instance);
    @memcpy(reply[43..][0..name.len], name);
    while (true) {
        var control: [128]u8 align(@alignOf(usize)) = undefined;
        const request = try receiveQuery(io, socket, &input, &control);
        // Every datagram consumes the budget, including malformed traffic and
        // queries arriving on an interface outside the configured bind.
        try io.sleep(.fromMilliseconds(20), .awake);
        if (!address.eql(&socket.address) and !selectedInterface(request, address)) continue;
        if (request.flags.trunc or request.data.len != 24 or !std.mem.eql(u8, request.data[0..8], query_magic)) continue;
        @memcpy(reply[8..24], request.data[8..24]);
        net_io.sendTimeout(responder, io, &request.from, reply[0 .. 43 + name.len], wire.timeout(1)) catch |err| {
            if (err == error.Canceled) return err;
        };
    }
}

fn receiveQuery(io: Io, socket: Io.net.Socket, input: []u8, control: []u8) !Io.net.IncomingMessage {
    if (!linux) return socket.receive(io, input);
    var message: Io.net.IncomingMessage = .init;
    message.control = control;
    const err, const count = (try io.operate(.{ .net_receive = .{
        .socket_handle = socket.handle,
        .message_buffer = (&message)[0..1],
        .data_buffer = input,
        .flags = .{},
    } })).net_receive;
    if (err) |failure| return failure;
    if (count != 1) return error.Unexpected;
    return message;
}

fn selectedInterface(message: Io.net.IncomingMessage, address: Io.net.IpAddress) bool {
    if (!linux or address != .ip4 or message.flags.ctrunc) return false;
    const Header = std.os.linux.cmsghdr;
    var offset: usize = 0;
    while (message.control.len - offset >= @sizeOf(Header)) {
        const header = std.mem.bytesToValue(Header, message.control[offset..][0..@sizeOf(Header)]);
        const data_offset = std.mem.alignForward(usize, @sizeOf(Header), @sizeOf(usize));
        if (header.len < data_offset or header.len > message.control.len - offset) return false;
        if (header.level == std.os.linux.IPPROTO.IP and header.type == std.os.linux.IP.PKTINFO) {
            if (header.len - data_offset < @sizeOf(std.os.linux.in_pktinfo)) return false;
            const info = std.mem.bytesToValue(std.os.linux.in_pktinfo, message.control[offset + data_offset ..][0..@sizeOf(std.os.linux.in_pktinfo)]);
            return std.mem.eql(u8, std.mem.asBytes(&info.spec_dst), &address.ip4.bytes);
        }
        const next = std.mem.alignForward(usize, header.len, @sizeOf(usize));
        if (next > message.control.len - offset) return false;
        offset += next;
    }
    return false;
}

pub fn find(a: std.mem.Allocator, io: Io, port: u16) ![]Peer {
    const address: Io.net.IpAddress = .{ .ip4 = .unspecified(0) };
    const socket = try address.bind(io, .{ .mode = .dgram, .protocol = .udp, .allow_broadcast = true });
    defer socket.close(io);
    var query: [24]u8 = undefined;
    @memcpy(query[0..8], query_magic);
    try io.randomSecure(query[8..24]);
    const broadcast: Io.net.IpAddress = .{ .ip4 = .{ .bytes = .{ 255, 255, 255, 255 }, .port = port } };
    const loopback: Io.net.IpAddress = .{ .ip4 = .loopback(port) };
    try net_io.sendTimeout(socket, io, &loopback, &query, wire.timeout(1));
    net_io.sendTimeout(socket, io, &broadcast, &query, wire.timeout(1)) catch |err| {
        if (err == error.Canceled) return err;
    };
    const deadline = wire.timeout(3).toDeadline(io);
    var peers: std.ArrayList(Peer) = .empty;
    var buffer: [256]u8 = undefined;
    while (peers.items.len < max_peers) {
        const reply = net_io.receiveTimeout(socket, io, &buffer, deadline) catch |err| switch (err) {
            error.Timeout => break,
            else => return err,
        };
        if (reply.flags.trunc) continue;
        const peer = parseReply(a, reply.data, query[8..24].*, reply.from) catch continue;
        var duplicate = false;
        for (peers.items) |known| {
            if (known.address.eql(&peer.address)) {
                duplicate = true;
                break;
            }
        }
        if (duplicate) {
            a.free(peer.name);
            continue;
        }
        try peers.append(a, peer);
    }
    return peers.toOwnedSlice(a);
}

fn parseReply(a: std.mem.Allocator, data: []const u8, nonce: [16]u8, from: Io.net.IpAddress) !Peer {
    if (data.len < 44 or !std.mem.eql(u8, data[0..8], reply_magic) or !std.mem.eql(u8, data[8..24], &nonce)) return error.InvalidAnnouncement;
    const len = data[26];
    if (len == 0 or len > 63 or data.len != 43 + @as(usize, len)) return error.InvalidAnnouncement;
    try paths.validateName(data[43..]);
    const port = std.mem.readInt(u16, data[24..26], .little);
    if (port == 0) return error.InvalidAnnouncement;
    var address = from;
    address.setPort(port);
    return .{ .name = try a.dupe(u8, data[43..]), .address = address, .instance = data[27..43].* };
}

test "discovery binds reply to nonce and packet source" {
    var packet: [47]u8 = undefined;
    @memcpy(packet[0..8], reply_magic);
    @memset(packet[8..24], 1);
    std.mem.writeInt(u16, packet[24..26], 9000, .little);
    packet[26] = 4;
    @memset(packet[27..43], 7);
    @memcpy(packet[43..], "desk");
    const peer = try parseReply(std.testing.allocator, &packet, @splat(1), .{ .ip4 = .loopback(123) });
    defer std.testing.allocator.free(peer.name);
    try std.testing.expectEqual(@as(u16, 9000), peer.address.getPort());
    try std.testing.expectError(error.InvalidAnnouncement, parseReply(std.testing.allocator, &packet, @splat(2), peer.address));
    packet[43] = 27;
    try std.testing.expectError(error.InvalidName, parseReply(std.testing.allocator, &packet, @splat(1), peer.address));
}

test "discovery wildcard binding requires Linux interface metadata" {
    const selected: Io.net.IpAddress = .{ .ip4 = .{ .bytes = .{ 192, 168, 1, 20 }, .port = 9000 } };
    const wildcard: Io.net.IpAddress = .{ .ip4 = .unspecified(9000) };
    try std.testing.expect(listenAddress(selected).eql(if (linux) &wildcard else &selected));
    const loopback: Io.net.IpAddress = .{ .ip4 = .loopback(9000) };
    try std.testing.expect(listenAddress(loopback).eql(&loopback));
}

test "malformed discovery datagrams consume the rate limit" {
    const Mock = struct {
        receives: usize = 0,
        sleeps: usize = 0,
        fn operate(context: ?*anyopaque, operation: Io.Operation) Io.Cancelable!Io.Operation.Result {
            const self: *@This() = @ptrCast(@alignCast(context.?));
            self.receives += 1;
            const receive = operation.net_receive;
            receive.message_buffer[0] = .{
                .from = .{ .ip4 = .loopback(1234) },
                .data = receive.data_buffer[0..0],
                .control = &.{},
                .flags = @bitCast(@as(u8, 0)),
            };
            return .{ .net_receive = .{ null, 1 } };
        }
        fn sleep(context: ?*anyopaque, _: Io.Timeout) Io.Cancelable!void {
            const self: *@This() = @ptrCast(@alignCast(context.?));
            self.sleeps += 1;
            return error.Canceled;
        }
    };
    var mock: Mock = .{};
    var vtable = std.testing.io.vtable.*;
    vtable.operate = Mock.operate;
    vtable.sleep = Mock.sleep;
    const io: Io = .{ .userdata = &mock, .vtable = &vtable };
    const address: Io.net.IpAddress = .{ .ip4 = .unspecified(9000) };
    const socket: Io.net.Socket = .{ .handle = undefined, .address = address };
    try std.testing.expectError(error.Canceled, serveInner(io, socket, address, 9000, "test", @splat(0)));
    try std.testing.expectEqual(@as(usize, 1), mock.receives);
    try std.testing.expectEqual(@as(usize, 1), mock.sleeps);
}

test "Linux discovery fails closed for absent or wrong interface metadata" {
    if (!linux) return;
    const Header = std.os.linux.cmsghdr;
    const data_offset = comptime std.mem.alignForward(usize, @sizeOf(Header), @sizeOf(usize));
    var control: [data_offset + @sizeOf(std.os.linux.in_pktinfo)]u8 = undefined;
    var message: Io.net.IncomingMessage = .init;
    message.flags = @bitCast(@as(u8, 0));
    const selected: Io.net.IpAddress = .{ .ip4 = .{ .bytes = .{ 192, 168, 1, 20 }, .port = 9000 } };
    try std.testing.expect(!selectedInterface(message, selected));
    const header: Header = .{ .len = control.len, .level = std.os.linux.IPPROTO.IP, .type = std.os.linux.IP.PKTINFO };
    @memcpy(control[0..@sizeOf(Header)], std.mem.asBytes(&header));
    var info: std.os.linux.in_pktinfo = .{ .ifindex = 2, .spec_dst = @bitCast(selected.ip4.bytes), .addr = @bitCast([4]u8{ 255, 255, 255, 255 }) };
    @memcpy(control[data_offset..], std.mem.asBytes(&info));
    message.control = &control;
    try std.testing.expect(selectedInterface(message, selected));
    info.spec_dst = @bitCast([4]u8{ 10, 0, 0, 1 });
    @memcpy(control[data_offset..], std.mem.asBytes(&info));
    try std.testing.expect(!selectedInterface(message, selected));
    message.flags.ctrunc = true;
    try std.testing.expect(!selectedInterface(message, selected));
}
