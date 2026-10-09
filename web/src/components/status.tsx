import { ArrowUpRight, ArrowDownLeft, CheckCircle2, LoaderCircle, CircleAlert, X } from "../icons";
import { useLive, useSession, selectTransfer } from "../session";
import { failure, message } from "../api";
import { size } from "../selection";
export function TransferStatus() {
  const state = useLive(selectTransfer);
  const { closed, cancel } = useSession();
  const data = state.data;
  if (!closed && !state.isError && (!data || data.phase === "ready")) return null;
  const complete = data?.phase === "sent" || data?.phase === "received";
  const failed = data?.phase === "failed" || state.isError;
  const text = closed
    ? "You can close this window. Launch XFER to share again."
    : state.isError
      ? "Connection interrupted. Retrying…"
      : data?.phase === "failed"
        ? failure(data.message)
        : data?.phase === "sent"
          ? "Sent " + data.message
          : data?.phase === "received"
            ? "Saved " + data.message
            : data?.message;
  const title = closed
    ? "XFER is closed"
    : state.isError
      ? "Reconnecting to XFER"
      : failed
        ? "Transfer couldn’t finish"
        : complete
          ? data?.phase === "received"
            ? "Files received"
            : "Delivery complete"
          : data?.phase === "uploading"
            ? "Preparing your files"
            : data?.phase === "approval"
              ? "Waiting for confirmation"
              : "Transfer in progress";
  const percent = data?.total ? Math.min(100, Math.round((data.bytes / data.total) * 100)) : 0;
  return (
    <section
      className={`transfer-status ${failed ? "failed" : complete ? "complete" : ""}`}
      id="status"
      aria-labelledby="transfer-title"
    >
      <div className="status-top">
        <span className="status-icon">
          {failed ? (
            <CircleAlert size={24} />
          ) : complete ? (
            <CheckCircle2 size={24} />
          ) : (
            <LoaderCircle className="spin" size={24} />
          )}
        </span>
        <div>
          <h2 id="transfer-title">{title}</h2>
          <p id="status-text" role="status">
            {text}
          </p>
        </div>
        {data?.busy && !closed && !state.isError && (
          <button id="cancel" disabled={cancel.isPending} onClick={() => cancel.mutate()}>
            <X size={14} />
            Cancel
          </button>
        )}
      </div>
      {data && data.total > 0 && !closed && !state.isError && (
        <div className="transfer-progress">
          <div className="progress-label">
            <span id="stats">
              {size(data.bytes)} / {size(data.total)}
            </span>
            <strong>{percent}%</strong>
          </div>
          <progress
            id="progress"
            aria-label="Transfer progress"
            max={data.total}
            value={Math.min(data.bytes, data.total)}
          />
          <div className="transfer-meta">
            {data.phase === "received" ? <ArrowDownLeft size={14} /> : <ArrowUpRight size={14} />}
            <span>
              {complete
                ? "Verified and delivered"
                : data.phase === "uploading"
                  ? "Preparing on this computer before sending"
                  : "Direct connection · Encrypted transfer"}
            </span>
          </div>
        </div>
      )}
      {cancel.isError && (
        <p className="error" role="alert">
          {message(cancel.error)}
        </p>
      )}
    </section>
  );
}
