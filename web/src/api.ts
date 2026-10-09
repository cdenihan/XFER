import type { Selection } from "./types";

const errors: Record<string, string> = {
  TailcatNotInstalled: "Install Tailcat 0.7+ on this computer and restart XFER.",
  TailcatStartupFailed:
    "Tailcat could not connect. Check your network or relay settings and try again.",
  TailcatStartupTimeout: "Tailcat took too long to start. Check your network and try again.",
  InvalidTailcatInvite: "Paste a complete XFER invitation with its original capitalization.",
  TailcatRequiresLoopbackListener: "Remote access requires XFER to listen on 0.0.0.0 or 127.0.0.1.",
  TailcatAlreadyRunning: "The remote connection is already starting or running.",
  Timeout: "The other computer did not respond. Check its firewall and try again.",
  ConnectionRefused: "Open XFER on the other computer and try again.",
  Declined: "The transfer was declined.",
  PeerRejected: "The transfer was declined.",
  TransferRejected: "The transfer was declined.",
  AuthenticationFailed: "The secure connection could not be verified.",
  TransferInProgress: "Another transfer is in progress.",
  InvalidName: "Rename this item so its name works on Windows, macOS, and Linux.",
  InvalidPath: "Rename this item so its name works on Windows, macOS, and Linux.",
  InvalidUpload: "These files exceed the limit or contain an invalid path.",
  RequestTooLarge: "The selection exceeds the transfer limit.",
  TransferTooLarge: "The selection exceeds the transfer limit.",
  Canceled: "Transfer canceled.",
  StaleApproval: "This request has expired.",
};
export const failure = (error: string) =>
  errors[error] ?? "The transfer could not finish. Check the other computer and try again.";
export const message = (error: unknown) =>
  error instanceof DOMException && error.name === "AbortError"
    ? "Transfer canceled."
    : error instanceof Error
      ? error.message === "Failed to fetch"
        ? "Connection interrupted. Check that XFER is still running."
        : error.message
      : "The request could not finish.";

export function takeCapability() {
  const fragment = location.hash.slice(1);
  const token = /^[a-f0-9]{64}$/.test(fragment)
    ? fragment
    : (sessionStorage.getItem("xfer-capability") ?? "");
  if (/^[a-f0-9]{64}$/.test(fragment)) {
    sessionStorage.setItem("xfer-capability", token);
    history.replaceState(null, "", location.pathname + location.search);
  }
  return token;
}
export function createApi(token: string) {
  return async function api<T = Record<string, never>>(
    path: string,
    body?: unknown,
    method = "POST",
    extraHeaders: Record<string, string> = {},
    signal?: AbortSignal,
  ): Promise<T> {
    const file = body instanceof File;
    const response = await fetch("/api/" + path, {
      method,
      signal,
      headers: {
        Authorization: "Bearer " + token,
        ...(file ? {} : { "Content-Type": "application/json" }),
        ...extraHeaders,
      },
      ...(body === undefined || method === "GET"
        ? {}
        : { body: file ? (body as File) : JSON.stringify(body) }),
    });
    const result = await response.json();
    if (!response.ok) throw new Error(failure(result.error));
    return result;
  };
}
export type Api = ReturnType<typeof createApi>;

// Only cancel staging after /new acknowledges ownership. A rejected /new can
// belong to an incoming transfer and must never cancel it. Writes are not retried.
export async function sendSelection(
  api: Api,
  selection: Selection,
  address: string,
  progress: (bytes: number) => void,
  signal?: AbortSignal,
) {
  let ownsSelection = false;
  try {
    signal?.throwIfAborted();
    await api("new", {
      name: selection.root,
      folder: selection.folder,
      total: selection.items.reduce((sum, item) => sum + (item.file?.size ?? 0), 0),
    });
    ownsSelection = true;
    signal?.throwIfAborted();
    let bytes = 0;
    for (const item of selection.items) {
      signal?.throwIfAborted();
      if (item.directory) await api("directory", { path: item.path }, "POST", {}, signal);
      else {
        await api(
          "upload",
          item.file,
          "PUT",
          { "X-Xfer-Path": encodeURIComponent(item.path) },
          signal,
        );
        bytes += item.file?.size ?? 0;
        progress(bytes);
      }
    }
    signal?.throwIfAborted();
    await api("send", { to: address });
    ownsSelection = false;
  } catch (error) {
    if (ownsSelection) {
      try {
        await api("cancel", {});
      } catch {
        /* Keep the original failure. */
      }
    }
    throw error;
  }
}
