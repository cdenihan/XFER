const std = @import("std");
const Io = std.Io;
const manifest = @import("manifest.zig");
const discovery = @import("discovery.zig");
const transfer = @import("transfer.zig");
const paths = @import("paths.zig");
const reporting = @import("reporter.zig");
pub const Reporter = reporting.Reporter;
pub const version = @import("build_options").version;
const Command = enum { desktop, menu, send, receive, discover, doctor, help, version };
pub const Config = struct {
    command: Command = .desktop,
    path: ?[]const u8 = null,
    to: ?[]const u8 = null,
    output: ?[]const u8 = null,
    bind: []const u8 = "0.0.0.0",
    name: ?[]const u8 = null,
    port: u16 = 9000,
    max_bytes: u64 = 16 * 1024 * 1024 * 1024,
    yes: bool = false,
    once: bool = false,
    no_discovery: bool = false,
    json: bool = false,
    dry_run: bool = false,
    no_open: bool = false,
};
const help =
    \\XFER — nearby file sharing for Windows, macOS and Linux
    \\
    \\Usage:
    \\  xfer                               Open the browser sharing window
    \\  xfer menu                          Open the terminal menu
    \\  xfer send PATH                     Choose a nearby receiver
    \\  xfer send PATH --to HOST            Send directly (IP or hostname)
    \\  xfer receive [--output DIR]         Accept incoming transfers
    \\  xfer discover                      List nearby receivers
    \\  xfer doctor                        Show build and network settings
    \\
    \\Options:
    \\  --port PORT            TCP and UDP port (default: 9000)
    \\  --name NAME            Receiver label (or XFER_NAME)
    \\  --bind ADDRESS         Receiver interface (default: 0.0.0.0)
    \\  --no-open              Print the browser URL without opening it
    \\  --once                 Receive one session and exit
    \\  --no-discovery         Disable receiver discovery
    \\  --max-bytes BYTES      Receive limit (default: 17179869184 / 16 GiB)
    \\  --dry-run              Inspect a send without connecting
    \\  --yes                  Approve automatically; requires XFER_TOKEN
    \\  --json                 Emit JSON events; use --yes for transfers
    \\  --help, -h             Show this help
    \\  --version              Show version
    \\
    \\Compare the displayed code on both devices before approving. For unattended
    \\use, set the same strong secret (at least 16 bytes) in XFER_TOKEN on both.
    \\Existing files are preserved. Symlinks and special files are skipped.
    \\
;

pub fn parse(args: []const []const u8) !Config {
    var config: Config = .{};
    var command_seen = false;
    var i: usize = 0;
    while (i < args.len) : (i += 1) {
        const arg = args[i];
        if (std.mem.eql(u8, arg, "--help") or std.mem.eql(u8, arg, "-h")) {
            config.command = .help;
            return config;
        }
        if (std.mem.eql(u8, arg, "--version")) {
            config.command = .version;
            return config;
        }
        if (std.mem.eql(u8, arg, "--json")) {
            config.json = true;
            continue;
        }
        if (std.mem.eql(u8, arg, "--yes")) {
            config.yes = true;
            continue;
        }
        if (std.mem.eql(u8, arg, "--once")) {
            config.once = true;
            continue;
        }
        if (std.mem.eql(u8, arg, "--no-discovery")) {
            config.no_discovery = true;
            continue;
        }
        if (std.mem.eql(u8, arg, "--no-open")) {
            config.no_open = true;
            continue;
        }
        if (std.mem.eql(u8, arg, "--dry-run")) {
            config.dry_run = true;
            continue;
        }
        if (std.mem.startsWith(u8, arg, "--")) {
            i += 1;
            if (i >= args.len) return error.MissingOptionValue;
            const value = args[i];
            if (std.mem.eql(u8, arg, "--to")) config.to = value else if (std.mem.eql(u8, arg, "--output")) config.output = value else if (std.mem.eql(u8, arg, "--bind")) config.bind = value else if (std.mem.eql(u8, arg, "--name")) config.name = value else if (std.mem.eql(u8, arg, "--port")) {
                config.port = std.fmt.parseInt(u16, value, 10) catch return error.InvalidPort;
                if (config.port == 0) return error.InvalidPort;
            } else if (std.mem.eql(u8, arg, "--max-bytes")) {
                config.max_bytes = std.fmt.parseInt(u64, value, 10) catch return error.InvalidLimit;
            } else return error.UnknownOption;
            continue;
        }
        if (!command_seen) {
            config.command = std.meta.stringToEnum(Command, arg) orelse return error.UnknownCommand;
            command_seen = true;
        } else if (config.command == .send and config.path == null) config.path = arg else return error.UnexpectedArgument;
    }
    if (config.command == .send and config.path == null) return error.PathRequired;
    if (config.command != .send and (config.to != null or config.dry_run)) return error.OptionNotApplicable;
    if (config.command != .receive and (config.once or config.no_discovery)) return error.OptionNotApplicable;
    if (config.command != .receive and config.command != .desktop and (config.output != null or config.name != null or !std.mem.eql(u8, config.bind, "0.0.0.0") or config.max_bytes != 16 * 1024 * 1024 * 1024)) return error.OptionNotApplicable;
    if (config.command != .desktop and config.no_open) return error.OptionNotApplicable;
    if (config.command == .desktop and config.yes) return error.OptionNotApplicable;
    return config;
}

pub fn run(init: std.process.Init) !void {
    const a = init.arena.allocator();
    const args = try init.minimal.args.toSlice(a);
    var config = try parse(args[1..]);
    const io = init.io;
    const reporter: Reporter = .{ .io = io, .json = config.json };
    if (config.command == .desktop) {
        return @import("desktop.zig").run(init, .{ .port = config.port, .bind = config.bind, .output = config.output orelse try defaultOutput(a, init), .name = config.name orelse try defaultName(a, init), .max_bytes = config.max_bytes, .open_browser = !config.no_open, .json = config.json });
    }
    if (config.command == .menu) {
        if (config.json or !try Io.File.stdin().isTty(io)) {
            try Io.File.stdout().writeStreamingAll(io, help);
            return;
        }
        try Io.File.stderr().writeStreamingAll(io, "\n  XFER\n  Share with a nearby computer\n\n  1  Send a file or folder\n  2  Receive files\n  3  Nearby computers\n  q  Quit\n\nChoose: ");
        var choice_buffer: [32]u8 = undefined;
        const choice = try reporting.readLine(io, &choice_buffer);
        if (std.mem.eql(u8, choice, "q")) return;
        if (std.mem.eql(u8, choice, "1")) {
            config.command = .send;
            try Io.File.stderr().writeStreamingAll(io, "File or folder path: ");
            var path_buffer: [paths.max_path]u8 = undefined;
            config.path = try a.dupe(u8, try reporting.readLine(io, &path_buffer));
        } else if (std.mem.eql(u8, choice, "2")) config.command = .receive else if (std.mem.eql(u8, choice, "3")) config.command = .discover else return error.InvalidSelection;
    }
    if (config.command == .help) {
        try Io.File.stdout().writeStreamingAll(io, help);
        return;
    }
    if (config.command == .version) {
        try Io.File.stdout().writeStreamingAll(io, "xfer " ++ version ++ " (Zig 0.17.0, protocol XFERZ017)\n");
        return;
    }
    if (config.command == .doctor) {
        try reporter.event("doctor", "XFER " ++ version ++ " | Zig " ++ @import("builtin").zig_version_string ++ " | " ++ @tagName(@import("builtin").os.tag) ++ "/" ++ @tagName(@import("builtin").cpu.arch), 0, 0);
        var buffer: [256]u8 = undefined;
        try reporter.event("doctor", try std.fmt.bufPrint(&buffer, "TCP + UDP port {d}. Allow XFER through your private-network firewall. Direct IPv4/IPv6; nearby discovery uses IPv4 broadcast.", .{config.port}), 0, 0);
        return;
    }
    if (config.command == .discover) {
        const peers = try discovery.find(a, io, config.port);
        try listPeers(a, reporter, peers);
        return;
    }
    const token = init.environ_map.get("XFER_TOKEN") orelse "";
    if (token.len > 1024) return error.TokenTooLong;
    if (token.len != 0 and token.len < 16) return error.SharedSecretTooShort;
    if (config.yes and token.len == 0) return error.SharedSecretRequired;
    if (config.json and !config.yes and !config.dry_run) return error.JsonRequiresAutomaticApproval;
    const options: transfer.Options = .{ .token = token, .yes = config.yes, .max_bytes = config.max_bytes, .reporter = reporter };
    if (config.command == .send) {
        try reporter.event("planning", "Preparing and hashing the item…", 0, 0);
        const plan = try manifest.plan(a, io, config.path.?);
        defer plan.close(io);
        var buffer: [1024]u8 = undefined;
        try reporter.event("planned", try std.fmt.bufPrint(&buffer, "{s}: {d} entries, {d} bytes, {d} symlinks or special files skipped", .{ plan.offer.name, plan.offer.entries.len, plan.offer.total, plan.skipped }), 0, plan.offer.total);
        if (config.dry_run) return;
        const host = config.to orelse try choosePeer(a, io, reporter, config.port);
        try reporter.event("connecting", host, 0, plan.offer.total);
        const stream = try transfer.connect(io, host, config.port);
        defer stream.close(io);
        try transfer.send(a, io, stream, plan, options);
    } else {
        const output_path = config.output orelse try defaultOutput(a, init);
        try Io.Dir.cwd().createDirPath(io, output_path);
        const output = try Io.Dir.cwd().openDir(io, output_path, .{});
        defer output.close(io);
        const address = try Io.net.IpAddress.resolve(io, config.bind, config.port);
        var server = try address.listen(io, .{});
        defer server.deinit(io);
        var background: Io.Group = .init;
        var discovery_socket: ?Io.net.Socket = null;
        defer if (discovery_socket) |socket| socket.close(io);
        defer background.cancel(io);
        if (!config.no_discovery and address == .ip4) {
            const name = config.name orelse try defaultName(a, init);
            if (name.len > 63) return error.NameTooLong;
            try paths.validateName(name);
            const socket = discovery.bind(io, address) catch |err| blk: {
                try reporter.event("warning", @errorName(err), 0, 0);
                break :blk null;
            };
            if (socket) |s| {
                discovery_socket = s;
                var instance: [16]u8 = undefined;
                try io.randomSecure(&instance);
                try background.concurrent(io, discovery.serve, .{ io, s, config.port, name, instance });
            }
        }
        try reporter.event("listening", try std.fmt.allocPrint(a, "Listening on {f}. Saving to {s}. Press Ctrl+C to stop.", .{ server.socket.address, output_path }), 0, 0);
        while (true) {
            const stream = try server.accept(io);
            defer stream.close(io);
            var arena: std.heap.ArenaAllocator = .init(init.gpa);
            defer arena.deinit();
            transfer.receive(arena.allocator(), io, stream, output, options) catch |err| {
                if (config.once) return err;
                try reporter.event("failed", @errorName(err), 0, 0);
                continue;
            };
            if (config.once) return;
        }
    }
}
fn defaultName(a: std.mem.Allocator, init: std.process.Init) ![]const u8 {
    if (init.environ_map.get("XFER_NAME")) |name| return name;
    if (init.environ_map.get("COMPUTERNAME") orelse init.environ_map.get("HOSTNAME")) |name| {
        if (name.len <= 63) {
            paths.validateName(name) catch return @tagName(@import("builtin").os.tag);
            return name;
        }
    }
    if (@import("builtin").os.tag != .windows) {
        var buffer: [std.posix.HOST_NAME_MAX]u8 = undefined;
        const name = std.posix.gethostname(&buffer) catch return @tagName(@import("builtin").os.tag);
        var length = @min(name.len, 63);
        while (length < name.len and length != 0 and name[length] & 0xc0 == 0x80) length -= 1;
        paths.validateName(name[0..length]) catch return @tagName(@import("builtin").os.tag);
        return a.dupe(u8, name[0..length]);
    }
    return "Windows PC";
}
fn defaultOutput(a: std.mem.Allocator, init: std.process.Init) ![]const u8 {
    const home = init.environ_map.get(if (@import("builtin").os.tag == .windows) "USERPROFILE" else "HOME") orelse return "./Received";
    return std.fs.path.join(a, &.{ home, "Downloads", "XFER" });
}
fn listPeers(a: std.mem.Allocator, reporter: Reporter, peers: []const discovery.Peer) !void {
    for (peers, 0..) |peer, i| {
        try reporter.event("peer", try std.fmt.allocPrint(a, "{d}. {s} — {f}", .{ i + 1, peer.name, peer.address }), 0, 0);
    }
    if (peers.len == 0) try reporter.event("discovery", "No receivers found. Start xfer receive on the other computer, or use --to HOST.", 0, 0);
}
fn choosePeer(a: std.mem.Allocator, io: Io, reporter: Reporter, port: u16) ![]const u8 {
    if (!try Io.File.stdin().isTty(io) or reporter.json) return error.DestinationRequired;
    try reporter.event("discovery", "Looking for nearby receivers…", 0, 0);
    const peers = try discovery.find(a, io, port);
    try listPeers(a, reporter, peers);
    try Io.File.stderr().writeStreamingAll(io, "Choose a number, or enter an address: ");
    var buffer: [512]u8 = undefined;
    const selected = try reporting.readLine(io, &buffer);
    if (std.fmt.parseInt(usize, selected, 10)) |index| {
        if (index == 0 or index > peers.len) return error.InvalidSelection;
        return std.fmt.allocPrint(a, "{f}", .{peers[index - 1].address});
    } else |_| {
        if (selected.len == 0) return error.DestinationRequired;
        return a.dupe(u8, selected);
    }
}

test "CLI rejects malformed arguments and incompatible option combinations" {
    const send_config = try parse(&.{ "--json", "send", "photo.jpg", "--to", "127.0.0.1", "--yes" });
    try std.testing.expectEqual(Command.send, send_config.command);
    try std.testing.expect(send_config.yes and send_config.json);
    for ([_][]const []const u8{ &.{"send"}, &.{ "send", "x", "--port", "0" }, &.{ "receive", "--to", "localhost" }, &.{ "receive", "--port", "65536" }, &.{"wat"}, &.{ "send", "x", "y" }, &.{ "send", "x", "--bad", "value" } }) |args| {
        try std.testing.expect(if (parse(args)) |_| false else |_| true);
    }
}
