//! Deadline adapters for network operations unsupported by Windows Io.Batch.
const std = @import("std");
const Io = std.Io;
const windows = @import("builtin").os.tag == .windows;

/// Windows' Threaded backend supports cancellable network operations, but not
/// network operations in awaitConcurrent batches. Race the operation against a
/// deadline instead. Cancel and join both tasks before borrowed buffers expire.
/// Other platforms retain the native batched timeout path without extra tasks.
pub inline fn operateTimeout(io: Io, operation: Io.Operation, deadline: Io.Timeout) Io.OperateTimeoutError!Io.Operation.Result {
    if (!windows) return io.operateTimeout(operation, deadline);
    if (deadline == .none) return io.operate(operation);
    return raceOperation(io, operation, deadline);
}
fn raceOperation(io: Io, operation: Io.Operation, deadline: Io.Timeout) Io.OperateTimeoutError!Io.Operation.Result {
    const Result = union(enum) { completed: Io.Cancelable!Io.Operation.Result, expired: Io.Cancelable!void };
    var results: [2]Result = undefined;
    var select = Io.Select(Result).init(io, &results);
    defer select.cancelDiscard();
    try select.concurrent(.completed, Io.operate, .{ io, operation });
    try select.concurrent(.expired, Io.Timeout.sleep, .{ deadline, io });
    return switch (try select.await()) {
        .completed => |result| result,
        .expired => error.Timeout,
    };
}

pub fn sendTimeout(socket: Io.net.Socket, io: Io, destination: *const Io.net.IpAddress, data: []const u8, deadline: Io.Timeout) Io.net.Socket.SendTimeoutError!void {
    if (!windows) return socket.sendTimeout(io, destination, data, deadline);
    var message: Io.net.OutgoingMessage = .{ .address = destination, .data_ptr = data.ptr, .data_len = data.len };
    const maybe_error, const count = (try operateTimeout(io, .{ .net_send = .{
        .socket_handle = socket.handle,
        .messages = (&message)[0..1],
        .flags = .{},
    } }, deadline)).net_send;
    if (maybe_error) |err| return err;
    if (count != 1) return error.Unexpected;
    if (message.data_len != data.len) return error.MessageOversize;
}

pub fn receiveTimeout(socket: Io.net.Socket, io: Io, buffer: []u8, deadline: Io.Timeout) Io.net.Socket.ReceiveTimeoutError!Io.net.IncomingMessage {
    if (!windows) return socket.receiveTimeout(io, buffer, deadline);
    var message: Io.net.IncomingMessage = .init;
    const maybe_error, const count = (try operateTimeout(io, .{ .net_receive = .{
        .socket_handle = socket.handle,
        .message_buffer = (&message)[0..1],
        .data_buffer = buffer,
        .flags = .{},
    } }, deadline)).net_receive;
    if (maybe_error) |err| return err;
    if (count != 1) return error.Unexpected;
    return message;
}
