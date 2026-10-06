//! XFER: direct, consent-based sharing over a local network.
pub const paths = @import("paths.zig");
pub const manifest = @import("manifest.zig");
pub const net_io = @import("net_io.zig");
pub const wire = @import("wire.zig");
pub const discovery = @import("discovery.zig");
pub const transfer = @import("transfer.zig");
pub const desktop = @import("desktop.zig");
pub const cli = @import("cli.zig");
test {
    @import("std").testing.refAllDecls(@This());
}
