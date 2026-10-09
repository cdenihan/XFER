import { defineConfig, type UserConfig, type PluginOption } from "vite-plus";
import react from "@vitejs/plugin-react";

const target = process.env.XFER_DEV_URL;
if (target && new URL(target).hostname !== "127.0.0.1")
  throw new Error("XFER_DEV_URL must be a loopback sharing URL");
const config: UserConfig = {
  plugins: [react()] as PluginOption[],
  test: { include: ["src/**/*.test.ts", "scripts/**/*.test.ts"] },
  build: { outDir: "dist", emptyOutDir: true },
  server: {
    host: "127.0.0.1",
    proxy: target
      ? {
          "/api": {
            target: new URL(target).origin,
            changeOrigin: true,
            configure(proxy) {
              proxy.on("proxyReq", (request) =>
                request.setHeader("Origin", new URL(target).origin),
              );
            },
          },
        }
      : undefined,
  },
};
export default defineConfig(config);
