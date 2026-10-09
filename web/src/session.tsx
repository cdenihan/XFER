import { createContext, useContext, useMemo, useRef, useState, type ReactNode } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createApi, takeCapability, sendSelection } from "./api";
import type { State, Peer, Selection } from "./types";

export const capability = takeCapability();
export const api = createApi(capability);
export const stateKey = ["state"] as const;
const queryFn = ({ signal }: { signal: AbortSignal }) =>
  api<State>("state", undefined, "GET", {}, signal);
export const selectIdentity = (state: State) => ({ name: state.name, busy: state.busy });
export const selectSharing = (state: State) => ({ busy: state.busy, peers: state.peers });
export const selectReceiving = (state: State) => ({
  name: state.name,
  output: state.output,
  busy: state.busy,
});
export const selectPending = (state: State) => state.pending;
export const selectTransfer = (state: State) => ({
  phase: state.phase,
  message: state.message,
  total: state.total,
  bytes: state.bytes,
  busy: state.busy,
});

function useSessionValue() {
  const client = useQueryClient();
  const [closed, setClosed] = useState(false);
  const [selection, setSelection] = useState<Selection | null>(null);
  const [transport, setTransport] = useState<"nearby" | "tailcat">("nearby");
  const [destination, setDestination] = useState<Peer | null>(null);
  const [reading, setReading] = useState(false);
  const lock = useRef(false);
  const controller = useRef<AbortController | null>(null);
  const summary = useMemo(() => {
    let total = 0,
      files = 0,
      directories = 0;
    for (const item of selection?.items ?? []) {
      total += item.file?.size ?? 0;
      if (item.file) files++;
      else directories++;
    }
    return { total, files, directories };
  }, [selection]);
  const send = useMutation({
    mutationFn: async ({ selected, peer }: { selected: Selection; peer: Peer }) => {
      controller.current = new AbortController();
      await sendSelection(api, selected, peer.address, () => {}, controller.current.signal);
    },
    onSuccess: () => setSelection(null),
    onSettled: () => {
      controller.current = null;
      lock.current = false;
      void client.invalidateQueries({ queryKey: stateKey });
    },
  });
  const quit = useMutation({
    mutationFn: () => api("quit", {}),
    onSuccess: async () => {
      setClosed(true);
      controller.current?.abort();
      await client.cancelQueries({ queryKey: stateKey });
    },
  });
  const cancel = useMutation({
    mutationFn: async () => {
      // Let the upload owner join the interrupted request and clean up staging.
      // Issuing an independent cancel while it runs races the next incoming job.
      if (controller.current) controller.current.abort();
      else await api("cancel", {});
    },
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: stateKey });
    },
  });
  return {
    closed,
    transport,
    setTransport,
    selection,
    setSelection,
    destination,
    setDestination,
    summary,
    reading,
    setReading,
    lock,
    send,
    quit,
    cancel,
  };
}
const Session = createContext<ReturnType<typeof useSessionValue> | null>(null);
function Poller({ closed }: { closed: boolean }) {
  useQuery({
    queryKey: stateKey,
    queryFn,
    enabled: !closed && !!capability,
    staleTime: Infinity,
    retry: false,
    refetchOnWindowFocus: false,
    refetchInterval: (query) =>
      query.state.status === "error"
        ? 1000
        : query.state.data?.busy ||
            query.state.data?.pending ||
            (query.state.data?.tailcat?.active && !query.state.data?.tailcat?.enabled)
          ? 300
          : 2000,
    refetchIntervalInBackground: true,
    notifyOnChangeProps: [],
  });
  return null;
}
export function SessionProvider({ children }: { children: ReactNode }) {
  const value = useSessionValue();
  return (
    <Session.Provider value={value}>
      <Poller closed={value.closed} />
      {children}
    </Session.Provider>
  );
}
export function useSession() {
  const value = useContext(Session);
  if (!value) throw new Error("Missing session provider");
  return value;
}
export function useLive<T>(select: (state: State) => T) {
  return useQuery({
    queryKey: stateKey,
    queryFn,
    select,
    enabled: false,
    staleTime: Infinity,
    retry: false,
    refetchOnWindowFocus: false,
    notifyOnChangeProps: ["data", "error", "status"],
  });
}
