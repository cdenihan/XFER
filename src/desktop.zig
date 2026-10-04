//! Loopback-only browser interface. All LAN file traffic uses the same verified
//! transfer core as the CLI. Browser capabilities never grant filesystem reads.
const std = @import("std");
const Io = std.Io;
const paths = @import("paths.zig");
const manifest = @import("manifest.zig");
const transfer = @import("transfer.zig");
const discovery = @import("discovery.zig");
const wire = @import("wire.zig");
const Reporter = @import("reporter.zig").Reporter;
pub const Settings = struct { port: u16 = 9000, bind: []const u8 = "0.0.0.0", output: []const u8, name: []const u8, max_bytes: u64 = 16 * 1024 * 1024 * 1024, open_browser: bool = true, json: bool = false };
const Peer = struct { name: []const u8, address: []const u8 };
const Pending = struct { id: u64, code: [12]u8, name: []const u8, total: u64, receiving: bool };
const Upload = struct {
    arena: std.heap.ArenaAllocator,
    path: []const u8,
    root: Io.Dir,
    name: []const u8,
    total: u64 = 0,
    count: usize = 0,
    in_use: bool = false,
    abort: ?*Io.Event = null,
    canceled: bool = false,
};
const State = struct {
    io: Io,
    gpa: std.mem.Allocator,
    settings: Settings,
    output: Io.Dir,
    tmp: []const u8,
    token: [64]u8,
    instance: [16]u8,
    shared_secret: []const u8,
    host: []const u8,
    mutex: Io.Mutex = .init,
    workers: Io.Group = .init,
    stop: Io.Event = .unset,
    decision_ready: Io.Event = .unset,
    decision: ?bool = null,
    pending: ?Pending = null,
    pending_name: [255]u8 = undefined,
    serial: u64 = 0,
    busy: bool = false,
    canceled: bool = false,
    cancel_transfer: Io.Event = .unset,
    phase: []const u8 = "ready",
    message: [1024]u8 = undefined,
    message_len: usize = 0,
    bytes: u64 = 0,
    total: u64 = 0,
    upload: ?*Upload = null,
    active: ?Io.net.Stream = null,
    peers: []Peer = &.{},
    peer_arena: std.heap.ArenaAllocator,
    connections: std.atomic.Value(u32) = .init(0),

    fn options(self: *State) transfer.Options {
        return .{ .token = self.shared_secret, .max_bytes = self.settings.max_bytes, .reporter = .{ .io = self.io, .sink = .{ .context = self, .emit = onEvent } }, .approval = .{ .context = self, .ask = askApproval } };
    }
    fn setMessage(self: *State, phase: []const u8, message: []const u8) void {
        self.phase = phase;
        self.message_len = @min(message.len, self.message.len);
        while (self.message_len < message.len and self.message_len != 0 and message[self.message_len] & 0xc0 == 0x80) self.message_len -= 1;
        @memcpy(self.message[0..self.message_len], message[0..self.message_len]);
    }
    fn finish(self: *State, result: anyerror!void) void {
        self.mutex.lockUncancelable(self.io);
        defer self.mutex.unlock(self.io);
        if (result) |_| {} else |err| self.setMessage("failed", @errorName(err));
        self.busy = false;
        self.active = null;
        self.pending = null;
    }
};

fn onEvent(context: *anyopaque, event: []const u8, message: []const u8, bytes: u64, total: u64) !void {
    const s: *State = @ptrCast(@alignCast(context));
    try s.mutex.lock(s.io);
    defer s.mutex.unlock(s.io);
    if (s.canceled) return error.Canceled;
    // Retain only known static event names; borrowed network text is copied.
    const phases = [_][]const u8{ "planning", "planned", "connecting", "offer", "accepted", "progress", "sent", "received", "failed" };
    for (phases) |phase| if (std.mem.eql(u8, phase, event)) {
        s.setMessage(phase, message);
        break;
    };
    s.bytes = bytes;
    s.total = total;
}
fn askApproval(context: *anyopaque, code: [12]u8, offer: manifest.Offer, receiving: bool) !bool {
    const s: *State = @ptrCast(@alignCast(context));
    try s.mutex.lock(s.io);
    if (s.canceled) {
        s.mutex.unlock(s.io);
        return false;
    }
    s.decision_ready.reset();
    s.decision = null;
    s.serial += 1;
    @memcpy(s.pending_name[0..offer.name.len], offer.name);
    s.pending = .{ .id = s.serial, .code = code, .name = s.pending_name[0..offer.name.len], .total = offer.total, .receiving = receiving };
    s.setMessage("approval", if (receiving) "Someone wants to share with you" else "Compare the code on the other computer");
    s.mutex.unlock(s.io);
    const deadline = wire.timeout(300).toDeadline(s.io);
    while (!s.decision_ready.isSet()) {
        s.decision_ready.waitTimeout(s.io, deadline) catch |err| {
            if (err != error.Timeout or deadline.toDurationFromNow(s.io).?.raw.nanoseconds <= 0) return err;
        };
    }
    try s.mutex.lock(s.io);
    defer s.mutex.unlock(s.io);
    const approved = s.decision orelse false;
    s.pending = null;
    return approved;
}

pub fn run(init: std.process.Init, settings: Settings) !void {
    const io = init.io;
    const a = init.arena.allocator();
    try paths.validateName(settings.name);
    if (settings.name.len > 63) return error.NameTooLong;
    try Io.Dir.cwd().createDirPath(io, settings.output);
    const output = try Io.Dir.cwd().openDir(io, settings.output, .{});
    defer output.close(io);
    const lan_address = try Io.net.IpAddress.resolve(io, settings.bind, settings.port);
    var lan = try lan_address.listen(io, .{});
    defer lan.deinit(io);
    const local_address: Io.net.IpAddress = .{ .ip4 = .loopback(0) };
    var local = try local_address.listen(io, .{});
    defer local.deinit(io);
    var random: [32]u8 = undefined;
    try io.randomSecure(&random);
    const token = std.fmt.bytesToHex(random, .lower);
    var instance: [16]u8 = undefined;
    try io.randomSecure(&instance);
    const host = try std.fmt.allocPrint(a, "127.0.0.1:{d}", .{local.socket.address.getPort()});
    const url = try std.fmt.allocPrint(a, "http://{s}/#{s}", .{ host, token });
    const shared = init.environ_map.get("XFER_TOKEN") orelse "";
    if (shared.len != 0 and (shared.len < 16 or shared.len > 1024)) return error.InvalidSharedSecret;
    var state: State = .{ .io = io, .gpa = init.gpa, .settings = settings, .output = output, .tmp = init.environ_map.get("TMPDIR") orelse init.environ_map.get("TEMP") orelse init.environ_map.get("TMP") orelse if (@import("builtin").os.tag == .windows) "." else "/tmp", .token = token, .instance = instance, .shared_secret = shared, .host = host, .peer_arena = .init(init.gpa) };
    state.setMessage("ready", "Choose files and a nearby computer");
    defer state.peer_arena.deinit();
    var udp: ?Io.net.Socket = null;
    defer if (udp) |socket| socket.close(io);
    defer {
        state.workers.cancel(io);
        if (state.upload) |upload| destroyUpload(&state, upload);
    }
    try state.workers.concurrent(io, lanLoop, .{ &state, &lan });
    try state.workers.concurrent(io, httpLoop, .{ &state, &local });
    try state.workers.concurrent(io, refreshPeers, .{&state});
    if (lan_address == .ip4) {
        udp = try discovery.bind(io, lan_address);
        try state.workers.concurrent(io, discovery.serve, .{ io, udp.?, settings.port, settings.name, instance });
    }
    const reporter: Reporter = .{ .io = io, .json = settings.json };
    try reporter.event("desktop", url, 0, 0);
    if (settings.open_browser) {
        openBrowser(io, a, url) catch |err| try reporter.event("warning", @errorName(err), 0, 0);
    }
    try state.stop.wait(io);
}
fn openBrowser(io: Io, a: std.mem.Allocator, url: []const u8) !void {
    const argv: []const []const u8 = switch (@import("builtin").os.tag) {
        .macos => &.{ "open", url },
        .windows => &.{ "cmd.exe", "/c", "start", "", url },
        else => &.{ "xdg-open", url },
    };
    const result = try std.process.run(a, io, .{ .argv = argv, .stdout_limit = .limited(4096), .stderr_limit = .limited(4096), .timeout = wire.timeout(10) });
    defer a.free(result.stdout);
    defer a.free(result.stderr);
    if (result.term != .exited or result.term.exited != 0) return error.BrowserLaunchFailed;
}
fn lanLoop(s: *State, server: *Io.net.Server) void {
    while (true) {
        const stream = server.accept(s.io) catch return;
        s.mutex.lockUncancelable(s.io);
        if (s.busy) {
            s.mutex.unlock(s.io);
            stream.close(s.io);
            continue;
        }
        s.busy = true;
        s.canceled = false;
        s.cancel_transfer.reset();
        s.active = stream;
        s.bytes = 0;
        s.total = 0;
        s.setMessage("connecting", "An incoming transfer is connecting");
        s.mutex.unlock(s.io);
        s.workers.concurrent(s.io, incoming, .{ s, stream }) catch {
            s.finish(error.SystemResources);
            stream.close(s.io);
        };
    }
}
fn incoming(s: *State, stream: Io.net.Stream) void {
    defer stream.close(s.io);
    var arena: std.heap.ArenaAllocator = .init(s.gpa);
    defer arena.deinit();
    s.finish(cancellableTransfer(s, transfer.receive, .{ arena.allocator(), s.io, stream, s.output, s.options() }));
}
fn refreshPeers(s: *State) void {
    while (true) {
        var arena: std.heap.ArenaAllocator = .init(s.gpa);
        var peers: std.ArrayList(Peer) = .empty;
        const found = discovery.find(arena.allocator(), s.io, s.settings.port) catch |err| {
            arena.deinit();
            if (err == error.Canceled) return;
            s.io.sleep(.fromSeconds(3), .awake) catch return;
            continue;
        };
        for (found) |peer| {
            // The LAN listener itself is not a useful receiving destination.
            if (std.mem.eql(u8, &peer.instance, &s.instance)) continue;
            const address = std.fmt.allocPrint(arena.allocator(), "{f}", .{peer.address}) catch continue;
            peers.append(arena.allocator(), .{ .name = peer.name, .address = address }) catch continue;
        }
        s.mutex.lockUncancelable(s.io);
        s.peer_arena.deinit();
        s.peer_arena = arena;
        s.peers = peers.toOwnedSlice(arena.allocator()) catch &.{};
        s.mutex.unlock(s.io);
        s.io.sleep(.fromSeconds(2), .awake) catch return;
    }
}
fn destroyUpload(s: *State, upload: *Upload) void {
    const old = s.io.swapCancelProtection(.blocked);
    defer _ = s.io.swapCancelProtection(old);
    upload.root.close(s.io);
    Io.Dir.cwd().deleteTree(s.io, upload.path) catch {};
    upload.arena.deinit();
    s.gpa.destroy(upload);
}
fn sendJob(s: *State, upload: *Upload, host: []const u8) void {
    defer destroyUpload(s, upload);
    s.finish(cancellableTransfer(s, sendJobInner, .{ s, upload, host }));
}
fn cancellableTransfer(s: *State, comptime task: anytype, args: anytype) !void {
    const Result = union(enum) { done: anyerror!void, canceled: Io.Cancelable!void };
    var results: [2]Result = undefined;
    var select = Io.Select(Result).init(s.io, &results);
    defer select.cancelDiscard();
    try select.concurrent(.done, task, args);
    try select.concurrent(.canceled, Io.Event.wait, .{ &s.cancel_transfer, s.io });
    return switch (try select.await()) {
        .done => |result| result,
        .canceled => error.Canceled,
    };
}
fn sendJobInner(s: *State, upload: *Upload, host: []const u8) !void {
    const a = upload.arena.allocator();
    try s.options().reporter.event("planning", "Preparing your files…", 0, upload.total);
    const path = try std.fs.path.join(a, &.{ upload.path, upload.name });
    const plan = try manifest.plan(a, s.io, path);
    defer plan.close(s.io);
    try s.options().reporter.event("connecting", "Connecting to the selected computer…", 0, plan.offer.total);
    const stream = try transfer.connect(s.io, host, s.settings.port);
    defer stream.close(s.io);
    try s.mutex.lock(s.io);
    if (s.canceled) {
        s.mutex.unlock(s.io);
        return error.Canceled;
    }
    s.active = stream;
    s.mutex.unlock(s.io);
    defer {
        // Remove the shared handle before closing it; cancel may run concurrently.
        s.mutex.lockUncancelable(s.io);
        s.active = null;
        s.mutex.unlock(s.io);
    }
    try transfer.send(a, s.io, stream, plan, s.options());
}
fn httpLoop(s: *State, server: *Io.net.Server) void {
    while (true) {
        const stream = server.accept(s.io) catch return;
        if (s.connections.fetchAdd(1, .acq_rel) >= 16) {
            _ = s.connections.fetchSub(1, .acq_rel);
            stream.close(s.io);
            continue;
        }
        s.workers.concurrent(s.io, httpConnection, .{ s, stream }) catch {
            _ = s.connections.fetchSub(1, .acq_rel);
            stream.close(s.io);
        };
    }
}
fn httpConnection(s: *State, stream: Io.net.Stream) void {
    defer {
        stream.close(s.io);
        _ = s.connections.fetchSub(1, .acq_rel);
    }
    const Result = union(enum) { done: anyerror!void, timeout: anyerror!void };
    var abort: Io.Event = .unset;
    var results: [2]Result = undefined;
    var select = Io.Select(Result).init(s.io, &results);
    defer select.cancelDiscard();
    select.concurrent(.done, httpInner, .{ s, stream, &abort }) catch return;
    select.concurrent(.timeout, Io.Event.waitTimeout, .{ &abort, s.io, wire.timeout(300) }) catch return;
    _ = select.await() catch return;
}
const response_headers = [_]std.http.Header{
    .{ .name = "Content-Type", .value = "application/json; charset=utf-8" },
    .{ .name = "Cache-Control", .value = "no-store" },
    .{ .name = "X-Content-Type-Options", .value = "nosniff" },
};
fn reply(request: *std.http.Server.Request, status: std.http.Status, body: []const u8) !void {
    try request.respond(body, .{ .status = status, .keep_alive = false, .extra_headers = &response_headers });
}
fn httpInner(s: *State, stream: Io.net.Stream, abort: *Io.Event) !void {
    var arena: std.heap.ArenaAllocator = .init(s.gpa);
    defer arena.deinit();
    const a = arena.allocator();
    var input_buffer: [16 * 1024]u8 = undefined;
    var output_buffer: [8192]u8 = undefined;
    var reader = stream.reader(s.io, &input_buffer);
    var writer = stream.writer(s.io, &output_buffer);
    var server = std.http.Server.init(&reader.interface, &writer.interface);
    var request = try server.receiveHead();
    var authorization: ?[]const u8 = null;
    var host: ?[]const u8 = null;
    var origin: ?[]const u8 = null;
    var upload_path: ?[]const u8 = null;
    var headers = request.iterateHeaders();
    while (headers.next()) |header| {
        if (std.ascii.eqlIgnoreCase(header.name, "authorization")) authorization = header.value;
        if (std.ascii.eqlIgnoreCase(header.name, "host")) host = header.value;
        if (std.ascii.eqlIgnoreCase(header.name, "origin")) origin = header.value;
        if (std.ascii.eqlIgnoreCase(header.name, "x-xfer-path")) upload_path = header.value;
    }
    if (host == null or !std.mem.eql(u8, host.?, s.host)) return reply(&request, .forbidden, "{\"error\":\"Invalid host\"}");
    if (origin) |value| {
        const expected = try std.fmt.allocPrint(a, "http://{s}", .{s.host});
        if (!std.mem.eql(u8, expected, value)) return reply(&request, .forbidden, "{\"error\":\"Invalid origin\"}");
    }
    if (request.head.method == .GET and std.mem.eql(u8, request.head.target, "/")) {
        return request.respond(@embedFile("ui/index.html"), .{ .keep_alive = false, .extra_headers = &.{
            .{ .name = "Content-Type", .value = "text/html; charset=utf-8" },
            .{ .name = "Cache-Control", .value = "no-store" },
            .{ .name = "Content-Security-Policy", .value = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src data:; base-uri 'none'; frame-ancestors 'none'; form-action 'none'" },
            .{ .name = "Referrer-Policy", .value = "no-referrer" },
        } });
    }
    if (request.head.method == .GET) {
        const asset: ?struct { body: []const u8, content_type: []const u8 } =
            if (std.mem.eql(u8, request.head.target, "/app.js")) .{ .body = @embedFile("ui/app.js"), .content_type = "text/javascript; charset=utf-8" } else if (std.mem.eql(u8, request.head.target, "/style.css")) .{ .body = @embedFile("ui/style.css"), .content_type = "text/css; charset=utf-8" } else null;
        if (asset) |value| return request.respond(value.body, .{ .keep_alive = false, .extra_headers = &.{
            .{ .name = "Content-Type", .value = value.content_type },
            .{ .name = "Cache-Control", .value = "no-store" },
            .{ .name = "X-Content-Type-Options", .value = "nosniff" },
        } });
    }
    const expected = try std.fmt.allocPrint(a, "Bearer {s}", .{s.token});
    if (authorization == null or !std.mem.eql(u8, authorization.?, expected)) return reply(&request, .unauthorized, "{\"error\":\"Launch XFER to open this window\"}");
    route(s, a, &request, upload_path, abort) catch |err| {
        // Cancellation is terminal; responding could retry a stalled body read.
        if (err == error.Canceled) return err;
        const body = try std.json.Stringify.valueAlloc(a, .{ .@"error" = @errorName(err) }, .{});
        try reply(&request, .bad_request, body);
    };
}
fn jsonBody(comptime T: type, a: std.mem.Allocator, request: *std.http.Server.Request) !std.json.Parsed(T) {
    if ((request.head.content_length orelse return error.LengthRequired) > 8192) return error.RequestTooLarge;
    var buffer: [8192]u8 = undefined;
    const reader = try request.readerExpectContinue(&buffer);
    const body = try reader.allocRemaining(a, .limited(8192));
    return std.json.parseFromSlice(T, a, body, .{ .allocate = .alloc_always });
}
fn route(s: *State, a: std.mem.Allocator, request: *std.http.Server.Request, path_header: ?[]const u8, abort: *Io.Event) !void {
    const target = try a.dupe(u8, request.head.target);
    if (request.head.method == .GET and std.mem.eql(u8, target, "/api/state")) {
        const body = blk: {
            try s.mutex.lock(s.io);
            defer s.mutex.unlock(s.io);
            break :blk try std.json.Stringify.valueAlloc(a, .{ .name = s.settings.name, .output = s.settings.output, .busy = s.busy, .phase = s.phase, .message = s.message[0..s.message_len], .bytes = s.bytes, .total = s.total, .pending = s.pending, .peers = s.peers }, .{});
        };
        return reply(request, .ok, body);
    }
    const expected_method: std.http.Method = if (std.mem.eql(u8, target, "/api/upload")) .PUT else .POST;
    if (request.head.method != expected_method) return reply(request, .method_not_allowed, "{}");
    if (std.mem.eql(u8, target, "/api/decision")) {
        const body = try jsonBody(struct { id: u64, approve: bool, code: []const u8 }, a, request);
        defer body.deinit();
        {
            try s.mutex.lock(s.io);
            defer s.mutex.unlock(s.io);
            const pending = s.pending orelse return error.NoPendingTransfer;
            if (body.value.id != pending.id or !std.mem.eql(u8, body.value.code, &pending.code)) return error.StaleApproval;
            if (s.decision != null) return error.AlreadyDecided;
            s.decision = body.value.approve;
            s.decision_ready.set(s.io);
        }
        return reply(request, .ok, "{}");
    }
    if (std.mem.eql(u8, target, "/api/cancel")) {
        {
            try s.mutex.lock(s.io);
            defer s.mutex.unlock(s.io);
            s.canceled = true;
            s.cancel_transfer.set(s.io);
            if (s.pending != null) {
                s.decision = false;
                s.decision_ready.set(s.io);
            }
            if (s.active) |active_stream| active_stream.shutdown(s.io, .both) catch {};
            if (s.upload) |upload| {
                upload.canceled = true;
                if (upload.abort) |upload_abort| upload_abort.set(s.io);
                if (!upload.in_use) {
                    s.upload = null;
                    destroyUpload(s, upload);
                    s.busy = false;
                }
            }
            s.setMessage("failed", "Transfer canceled");
        }
        return reply(request, .ok, "{}");
    }
    if (std.mem.eql(u8, target, "/api/quit")) {
        try reply(request, .ok, "{}");
        s.stop.set(s.io);
        return;
    }
    if (std.mem.eql(u8, target, "/api/new")) {
        const body = try jsonBody(struct { name: []const u8, folder: bool = false, total: u64 = 0 }, a, request);
        defer body.deinit();
        try paths.validateName(body.value.name);
        if (body.value.total > s.settings.max_bytes) return error.TransferTooLarge;
        {
            try s.mutex.lock(s.io);
            defer s.mutex.unlock(s.io);
            if (s.busy) return error.TransferInProgress;
            const upload = try s.gpa.create(Upload);
            errdefer s.gpa.destroy(upload);
            upload.* = .{ .arena = .init(s.gpa), .path = "", .root = undefined, .name = "" };
            errdefer upload.arena.deinit();
            var random: [16]u8 = undefined;
            try s.io.randomSecure(&random);
            const directory = try std.fmt.allocPrint(upload.arena.allocator(), "xfer-upload-{s}", .{std.fmt.bytesToHex(random, .lower)});
            upload.path = try std.fs.path.join(upload.arena.allocator(), &.{ s.tmp, directory });
            try Io.Dir.cwd().createDir(s.io, upload.path, privateDirPermissions());
            errdefer Io.Dir.cwd().deleteTree(s.io, upload.path) catch {};
            upload.root = try Io.Dir.cwd().openDir(s.io, upload.path, .{ .follow_symlinks = false });
            errdefer upload.root.close(s.io);
            upload.name = try upload.arena.allocator().dupe(u8, body.value.name);
            if (body.value.folder) try upload.root.createDir(s.io, upload.name, privateDirPermissions());
            s.upload = upload;
            s.busy = true;
            s.canceled = false;
            s.cancel_transfer.reset();
            s.bytes = 0;
            s.total = body.value.total;
            s.setMessage("uploading", "Preparing selected files");
        }
        return reply(request, .ok, "{}");
    }
    if (std.mem.eql(u8, target, "/api/directory")) {
        const body = try jsonBody(struct { path: []const u8 }, a, request);
        defer body.deinit();
        try paths.validate(body.value.path);
        {
            try s.mutex.lock(s.io);
            defer s.mutex.unlock(s.io);
            const upload = s.upload orelse return error.NoSelectedFiles;
            if (upload.in_use or upload.canceled or upload.count >= manifest.max_entries) return error.InvalidUpload;
            const path = body.value.path;
            if (!(std.mem.eql(u8, path, upload.name) or (path.len > upload.name.len and std.mem.startsWith(u8, path, upload.name) and path[upload.name.len] == '/'))) return error.InvalidUpload;
            try upload.root.createDirPath(s.io, path);
            upload.count += 1;
        }
        return reply(request, .ok, "{}");
    }
    if (std.mem.eql(u8, target, "/api/upload")) return uploadFile(s, a, request, path_header orelse return error.PathRequired, abort);
    if (std.mem.eql(u8, target, "/api/send")) {
        const body = try jsonBody(struct { to: []const u8 }, a, request);
        defer body.deinit();
        if (body.value.to.len == 0 or body.value.to.len > 512) return error.InvalidAddress;
        {
            try s.mutex.lock(s.io);
            defer s.mutex.unlock(s.io);
            const upload = s.upload orelse return error.NoSelectedFiles;
            if (upload.in_use or upload.canceled) return error.TransferInProgress;
            const host = try upload.arena.allocator().dupe(u8, body.value.to);
            try s.workers.concurrent(s.io, sendJob, .{ s, upload, host });
            s.upload = null;
        }
        return reply(request, .ok, "{}");
    }
    return reply(request, .not_found, "{}");
}
fn uploadFile(s: *State, a: std.mem.Allocator, request: *std.http.Server.Request, encoded: []const u8, abort: *Io.Event) !void {
    const path = try decodePath(a, encoded);
    try paths.validate(path);
    const size = request.head.content_length orelse return error.LengthRequired;
    try s.mutex.lock(s.io);
    const upload = s.upload orelse {
        s.mutex.unlock(s.io);
        return error.NoSelectedFiles;
    };
    if (upload.in_use or upload.canceled or upload.count >= manifest.max_entries or size > s.settings.max_bytes - upload.total or
        !(std.mem.eql(u8, path, upload.name) or (path.len > upload.name.len and std.mem.startsWith(u8, path, upload.name) and path[upload.name.len] == '/')))
    {
        s.mutex.unlock(s.io);
        return error.InvalidUpload;
    }
    upload.in_use = true;
    upload.abort = abort;
    s.mutex.unlock(s.io);
    var succeeded = false;
    defer {
        s.mutex.lockUncancelable(s.io);
        upload.in_use = false;
        upload.abort = null;
        if (!succeeded or upload.canceled) {
            s.upload = null;
            s.busy = false;
            s.setMessage("failed", "File preparation canceled. Select the files again.");
            destroyUpload(s, upload);
        }
        s.mutex.unlock(s.io);
    }
    if (std.mem.lastIndexOfScalar(u8, path, '/')) |slash| try upload.root.createDirPath(s.io, path[0..slash]);
    const parent = try paths.parent(upload.root, s.io, path);
    defer parent.close(s.io);
    const file = try parent.dir.createFile(s.io, parent.name, .{ .exclusive = true, .permissions = privateFilePermissions() });
    defer file.close(s.io);
    var body_buffer: [65536]u8 = undefined;
    const reader = try request.readerExpectContinue(&body_buffer);
    var bytes: [65536]u8 = undefined;
    var offset: u64 = 0;
    while (offset < size) {
        const chunk = bytes[0..@min(bytes.len, size - offset)];
        try reader.readSliceAll(chunk);
        try file.writePositionalAll(s.io, chunk, offset);
        offset += chunk.len;
        try s.mutex.lock(s.io);
        const canceled = upload.canceled;
        s.bytes = upload.total + offset;
        s.mutex.unlock(s.io);
        if (canceled) return error.Canceled;
    }
    try s.mutex.lock(s.io);
    upload.total += size;
    upload.count += 1;
    s.mutex.unlock(s.io);
    succeeded = true;
    try reply(request, .ok, "{}");
}
fn decodePath(a: std.mem.Allocator, encoded: []const u8) ![]u8 {
    if (encoded.len > paths.max_path * 3) return error.InvalidPath;
    var output: std.ArrayList(u8) = .empty;
    var i: usize = 0;
    while (i < encoded.len) : (i += 1) {
        if (encoded[i] == '%') {
            if (i + 2 >= encoded.len) return error.InvalidPath;
            try output.append(a, std.fmt.parseInt(u8, encoded[i + 1 ..][0..2], 16) catch return error.InvalidPath);
            i += 2;
        } else try output.append(a, encoded[i]);
    }
    return output.toOwnedSlice(a);
}
fn privateDirPermissions() Io.File.Permissions {
    return if (@import("builtin").os.tag == .windows) .default_dir else .fromMode(0o700);
}
fn privateFilePermissions() Io.File.Permissions {
    return if (@import("builtin").os.tag == .windows) .default_file else .fromMode(0o600);
}

test "browser paths decode strictly before portable validation" {
    var arena: std.heap.ArenaAllocator = .init(std.testing.allocator);
    defer arena.deinit();
    const path = try decodePath(arena.allocator(), "photos/%C3%A9t%C3%A9.jpg");
    try paths.validate(path);
    try std.testing.expectError(error.InvalidName, paths.validate(try decodePath(arena.allocator(), "photos/%2E%2E/escape")));
    try std.testing.expectError(error.InvalidPath, decodePath(arena.allocator(), "%2"));
}
