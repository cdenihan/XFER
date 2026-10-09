import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Wifi, ArrowUpRight, ShieldCheck, Check } from "../icons";
import { api, stateKey, useLive, useSession } from "../session";
import { message } from "../api";
import type { State } from "../types";
const selectRemote = (state: State) =>
  state.tailcat ?? { available: false, active: false, enabled: false, invite: "", error: "" };
export function TransportChoice() {
  const { transport, setTransport, setDestination, closed, send, reading } = useSession();
  const remote = useLive(selectRemote);
  const disabled = closed || send.isPending || reading;
  return (
    <div className="transport-choice" role="group" aria-label="Transfer backend">
      {(["nearby", "tailcat"] as const).map((mode) => (
        <button
          key={mode}
          className={transport === mode ? "active" : ""}
          aria-pressed={transport === mode}
          disabled={disabled}
          onClick={() => {
            if (mode !== transport) {
              setDestination(null);
              setTransport(mode);
            }
          }}
        >
          {mode === "nearby" ? <Wifi size={18} /> : <ArrowUpRight size={18} />}
          <span>
            <strong>{mode === "nearby" ? "Nearby" : "Across a distance"}</strong>
            <small>
              {mode === "nearby" ? "Direct on your local network" : "Tailcat · No account needed"}
            </small>
          </span>
          <span className="transport-check">{transport === mode && <Check size={12} />}</span>
        </button>
      ))}
      <p>
        {transport === "nearby"
          ? "Nearby computers appear automatically."
          : remote.data?.available
            ? "Tailcat helper detected. XFER still verifies and approves every transfer."
            : "Tailcat 0.7+ is required on both computers. Install it to use remote sharing."}
      </p>
    </div>
  );
}
export function RemoteDestination({ disabled }: { disabled: boolean }) {
  const remote = useLive(selectRemote);
  const { destination, setDestination } = useSession();
  const [invite, setInvite] = useState(() =>
    destination?.address.startsWith("xfer-tailcat:") ? destination.address : "",
  );
  const [error, setError] = useState("");
  return (
    <section className="panel remote-destination">
      <div className="panel-heading">
        <div>
          <span className="step-number">02</span>
          <h2>Connect to their computer</h2>
        </div>
        <ArrowUpRight size={18} />
      </div>
      <p className="panel-description">
        Ask them to open Receive, enable remote access, and send you their invitation.
      </p>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          const value = invite.trim();
          if (
            !/^xfer-tailcat:[1-9]\d{0,4}:tc[A-Za-z0-9_-]{38,2046}$/.test(value) ||
            Number(value.split(":")[1]) > 65535
          ) {
            setError("Paste the complete XFER invitation, keeping its original capitalization.");
            return;
          }
          setError("");
          setDestination({ name: "Remote computer", address: value });
        }}
      >
        <label htmlFor="remote-invite">Their invitation</label>
        <textarea
          id="remote-invite"
          placeholder="xfer-tailcat:9000:tc…"
          value={invite}
          onChange={(event) => {
            const value = event.target.value;
            setInvite(value);
            if (value.trim() !== destination?.address) setDestination(null);
          }}
          disabled={disabled || !remote.data?.available}
          rows={4}
          autoComplete="off"
          spellCheck={false}
        />
        <button
          className="primary"
          id="remote-connect"
          disabled={disabled || !remote.data?.available || !invite.trim()}
        >
          Connect with invitation
          <ArrowUpRight size={16} />
        </button>
      </form>
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      {destination && (
        <div className="destination-chip">
          <Check size={14} />
          Remote computer selected
        </div>
      )}
      <p className="remote-note">
        <ShieldCheck size={15} />
        You’ll compare a code before files leave either computer.
      </p>
    </section>
  );
}
export function RemoteReceiver() {
  const remote = useLive(selectRemote);
  const { closed } = useSession();
  const client = useQueryClient();
  const [copied, setCopied] = useState(false);
  const [copyError, setCopyError] = useState("");
  const toggle = useMutation({
    mutationFn: (start: boolean) => api(start ? "tailcat/start" : "tailcat/stop", {}),
    onSuccess: () => {
      setCopied(false);
      void client.invalidateQueries({ queryKey: stateKey });
    },
  });
  const data = remote.data;
  return (
    <section className="panel remote-receiver">
      <div className="remote-receiver-heading">
        <div>
          <p className="eyebrow">RECEIVE FROM ANYWHERE</p>
          <h2>{data?.enabled ? "Your invitation is ready." : "Make distance disappear."}</h2>
          <p>Enable a temporary Tailcat connection until you turn it off or quit.</p>
        </div>
        <button
          id="remote-toggle"
          disabled={
            closed || toggle.isPending || !data?.available || (!!data?.active && !data.enabled)
          }
          onClick={() => toggle.mutate(!data?.enabled)}
        >
          {data?.enabled
            ? "Turn off remote access"
            : data?.active
              ? "Starting Tailcat…"
              : "Enable remote access"}
          <ArrowUpRight size={16} />
        </button>
      </div>
      {!data?.available && (
        <p className="helper-missing">
          Install Tailcat 0.7+ and restart XFER. On macOS: <code>brew install tailcat</code>
        </p>
      )}
      {data?.enabled && (
        <div className="invitation-box">
          <label htmlFor="your-invite">Share privately with the person sending files</label>
          <div>
            <textarea id="your-invite" readOnly value={data.invite} rows={3} spellCheck={false} />
            <button
              id="copy-invite"
              onClick={async () => {
                try {
                  await navigator.clipboard.writeText(data.invite);
                  setCopied(true);
                  setCopyError("");
                } catch {
                  setCopyError("Select and copy the invitation above.");
                }
              }}
            >
              {copied ? "Copied" : "Copy invitation"}
            </button>
          </div>
          <p>
            Valid until you turn off remote access or quit XFER. You still approve each transfer.
          </p>
          {copyError && <p role="status">{copyError}</p>}
        </div>
      )}
      {(toggle.isError || data?.error) && (
        <p className="error" role="alert">
          {toggle.isError
            ? message(toggle.error)
            : "Tailcat could not stay connected. Check your network or relay settings, then try again."}
        </p>
      )}
    </section>
  );
}
