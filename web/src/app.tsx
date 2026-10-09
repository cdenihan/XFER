import "./style.css";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import {
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
  Outlet,
  Link,
} from "@tanstack/react-router";
import { ArrowUpRight, ArrowDownLeft, Monitor, Power, ShieldCheck, Wifi } from "./icons";
import { capability, SessionProvider, useSession, useLive, selectIdentity } from "./session";
import { Share } from "./components/share";
import { Receive } from "./components/receive";
import { Approval } from "./components/approval";
import { TransferStatus } from "./components/status";
import { message } from "./api";

function DeviceIdentity() {
  const state = useLive(selectIdentity);
  const { closed } = useSession();
  const status = closed
    ? "XFER is closed"
    : state.isError
      ? "Reconnecting…"
      : !state.data
        ? "Connecting…"
        : state.data.busy
          ? "Sharing in progress"
          : "Ready to receive";
  return (
    <div className="identity">
      <Monitor size={18} />
      <div>
        <strong id="identity">{state.data?.name ?? "This computer"}</strong>
        <span id="online" className={closed || state.isError ? "offline" : ""} role="status">
          <i className="dot" />
          {status}
        </span>
      </div>
    </div>
  );
}
function Shell() {
  const { quit, closed } = useSession();
  return (
    <div className="app-shell">
      <a className="skip-link" href="#main">
        Skip to content
      </a>
      <aside className="app-header">
        <Link to="/" className="brand" aria-label="XFER home">
          <span className="brand-mark">
            <ArrowUpRight size={24} />
          </span>
          XFER<span className="brand-label">A LITTLE CLOSER</span>
        </Link>
        <p className="sidebar-label">YOUR TRANSFER SPACE</p>
        <nav aria-label="Main navigation">
          <Link to="/" activeOptions={{ exact: true }} activeProps={{ className: "active" }}>
            <ArrowUpRight size={18} />
            Share files
          </Link>
          <Link to="/receive" activeProps={{ className: "active" }}>
            <ArrowDownLeft size={18} />
            Receive
          </Link>
        </nav>
        <div className="sidebar-story">
          <ShieldCheck size={24} />
          <strong>
            Far away.
            <br />
            Still just between you.
          </strong>
          <p>
            Private connections.
            <br />
            Verified deliveries.
          </p>
        </div>
        <DeviceIdentity />
      </aside>
      <div className="workspace">
        <header className="workspace-bar">
          <span>YOUR FILES, ON THEIR WAY</span>
          <span>
            <ShieldCheck size={14} />
            Encrypted &amp; verified
          </span>
        </header>
        <main id="main" tabIndex={-1}>
          <Outlet />
          <TransferStatus />
        </main>
        <footer>
          <span>
            <ShieldCheck size={15} />
            End-to-end encrypted<span className="footer-dot">·</span>
            <Wifi size={15} />
            Local network
          </span>
          <button
            className="quiet"
            id="quit"
            disabled={closed || quit.isPending}
            onClick={() => quit.mutate()}
          >
            <Power size={14} />
            {quit.isPending ? "Closing…" : "Quit XFER"}
          </button>
        </footer>
        {quit.isError && (
          <p className="error footer-error" role="alert">
            {message(quit.error)}
          </p>
        )}
      </div>
      <Approval />
      {!capability && (
        <div className="launch-message" role="alert">
          <strong>Open this window from XFER.</strong>
          <p>Launch the XFER executable to connect securely to your computer.</p>
        </div>
      )}
    </div>
  );
}
const root = createRootRoute({
  component: Shell,
  notFoundComponent: () => (
    <section className="page-heading">
      <h1>Page not found</h1>
      <Link to="/">Return to sharing</Link>
    </section>
  ),
});
const share = createRoute({ getParentRoute: () => root, path: "/", component: Share });
const receive = createRoute({ getParentRoute: () => root, path: "/receive", component: Receive });
const router = createRouter({
  routeTree: root.addChildren([share, receive]),
  defaultStructuralSharing: true,
});
declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}
const client = new QueryClient({
  defaultOptions: {
    queries: { staleTime: Infinity, retry: false, refetchOnWindowFocus: false },
    mutations: { retry: false },
  },
});
createRoot(document.getElementById("root")!).render(
  <QueryClientProvider client={client}>
    <SessionProvider>
      <RouterProvider router={router} />
    </SessionProvider>
  </QueryClientProvider>,
);
