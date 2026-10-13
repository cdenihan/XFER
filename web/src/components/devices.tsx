import { useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Monitor, Check, RefreshCw, Radar, ChevronDown, ArrowRight } from "../icons";
import { stateKey, useLive, useSession, selectSharing } from "../session";
export function Devices({ disabled }: { disabled: boolean }) {
  const state = useLive(selectSharing);
  const { destination, setDestination, closed } = useSession();
  const [address, setAddress] = useState("");
  const client = useQueryClient();
  return (
    <section className="panel device-panel" aria-labelledby="devices-title">
      <div className="panel-heading">
        <div>
          <span className="step-number">02</span>
          <h2 id="devices-title">Choose a computer</h2>
        </div>
        <button
          className="icon-button"
          title="Refresh nearby computers"
          aria-label="Refresh nearby computers"
          disabled={closed}
          onClick={() => {
            void client.invalidateQueries({ queryKey: stateKey });
          }}
        >
          <RefreshCw size={15} />
        </button>
      </div>
      <p className="panel-description">Open XFER on a computer on the same network.</p>
      <div className="discovery-label" id="scanning">
        <i className="dot" />
        {state.isError ? "Reconnecting" : `${state.data?.peers.length ?? 0} computers nearby`}
      </div>
      <div className="peers" id="peers">
        {state.data?.peers.map((peer) => (
          <button
            key={peer.address}
            className={`peer ${destination?.address === peer.address ? "selected" : ""}`}
            disabled={disabled}
            aria-pressed={destination?.address === peer.address}
            onClick={() => setDestination(peer)}
          >
            <span className="device-icon">
              <Monitor size={24} strokeWidth={1.5} />
            </span>
            <span className="peer-copy">
              <strong>{peer.name}</strong>
              <small>{peer.address}</small>
            </span>
            <span className="peer-check">
              {destination?.address === peer.address ? <Check size={14} /> : null}
            </span>
          </button>
        ))}
        {!state.data?.peers.length && (
          <div className="empty-devices">
            <span className="radar-art">
              <Radar size={33} strokeWidth={1.2} />
            </span>
            <strong>{state.isError ? "Connection interrupted" : "Looking for company"}</strong>
            <p>
              {state.isError
                ? "Keep XFER running. We’ll reconnect automatically."
                : "Your other computer will appear here when XFER is open."}
            </p>
          </div>
        )}
      </div>
      <details className="manual-connect">
        <summary>
          Use an IP address instead
          <ChevronDown size={15} />
        </summary>
        <form
          onSubmit={(event) => {
            event.preventDefault();
            if (!disabled && address.trim())
              setDestination({ name: address.trim(), address: address.trim() });
          }}
        >
          <label htmlFor="address">Computer IP address or hostname</label>
          <div className="manual">
            <input
              id="address"
              value={address}
              placeholder="192.168.1.42:9000"
              disabled={disabled}
              onChange={(event) => setAddress(event.target.value)}
              autoComplete="off"
              spellCheck={false}
            />
            <button
              id="connect"
              disabled={disabled || !address.trim()}
              aria-label="Choose this address"
            >
              <ArrowRight size={17} />
            </button>
          </div>
          <p>Useful when your network blocks nearby discovery.</p>
        </form>
      </details>
      {destination && (
        <div className="destination-chip">
          <Check size={14} />
          <span>
            Selected: <strong>{destination.name}</strong>
          </span>
        </div>
      )}
    </section>
  );
}
