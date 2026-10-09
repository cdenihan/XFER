import { useCallback, useRef, useState } from "react";
import { ArrowUpRight, FilePlus2, FolderPlus, Upload, ShieldCheck, Check, Files } from "../icons";
import { useLive, useSession } from "../session";
import { message } from "../api";
import { fromDrop, fromFiles, size } from "../selection";
import type { Selection, State } from "../types";
import { FileQueue } from "./file-queue";
import { TransportChoice, RemoteDestination } from "./transport";
import { Devices } from "./devices";
const selectBusy = (state: State) => state.busy;
export function Share() {
  const state = useLive(selectBusy);
  const {
    closed,
    transport,
    selection,
    setSelection,
    destination,
    summary,
    reading,
    setReading,
    lock,
    send,
  } = useSession();
  const [dragging, setDragging] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const files = useRef<HTMLInputElement>(null);
  const folder = useRef<HTMLInputElement>(null);
  const disabled =
    closed ||
    state.isError ||
    state.data === undefined ||
    !!state.data ||
    send.isPending ||
    reading;
  const choose = useCallback(
    (value: Selection | null) => {
      if (disabled || lock.current) return;
      setSelection(value);
      setError(null);
      send.reset();
    },
    [disabled, lock, setSelection, send],
  );
  const step = send.isPending || state.data ? 3 : selection ? (destination ? 3 : 2) : 1;
  return (
    <>
      <div className="page-heading">
        <div>
          <p className="eyebrow">GOOD FILES. NO BORDERS.</p>
          <h1>Send without borders.</h1>
          <p>Choose your files and how they travel. We’ll take care of the safe arrival.</p>
        </div>
        <span className="heading-badge">
          <ShieldCheck size={15} />
          Private by design
        </span>
      </div>
      <TransportChoice />
      <ol className="workflow" aria-label="Sharing steps">
        {["Select your files", "Choose a computer", "Compare & share"].map((label, index) => (
          <li
            key={label}
            className={step === index + 1 ? "current" : step > index + 1 ? "done" : ""}
          >
            <span>{step > index + 1 ? <Check size={13} /> : index + 1}</span>
            {label}
          </li>
        ))}
      </ol>
      <div className="share-layout">
        <section className="panel selection-panel" aria-labelledby="selection-title">
          <div className="panel-heading">
            <div>
              <span className="step-number">01</span>
              <h2 id="selection-title">What are you sending?</h2>
            </div>
            <Files size={18} />
          </div>
          <div
            className={`drop-zone ${dragging ? "dragging" : ""} ${selection ? "has-selection" : ""}`}
            id="drop"
            aria-label="File selection"
            aria-busy={reading || send.isPending}
            onDragOver={(event) => {
              event.preventDefault();
              if (!disabled) setDragging(true);
            }}
            onDragLeave={(event) => {
              if (!event.currentTarget.contains(event.relatedTarget as Node)) setDragging(false);
            }}
            onDrop={async (event) => {
              event.preventDefault();
              setDragging(false);
              if (disabled || lock.current) return;
              const data = event.dataTransfer;
              lock.current = true;
              setReading(true);
              try {
                setSelection(await fromDrop(data));
                setError(null);
                send.reset();
              } catch (failure) {
                setError(message(failure));
              } finally {
                lock.current = false;
                setReading(false);
              }
            }}
          >
            <div className="upload-art" aria-hidden="true">
              <span>
                <Upload size={28} strokeWidth={1.5} />
              </span>
              <i className="art-file">↗</i>
            </div>
            <h3 id="selection">
              {reading
                ? "Reading your folder…"
                : selection
                  ? selection.root
                  : "Drop something here"}
            </h3>
            <p id="selection-detail">
              {selection
                ? `${summary.files} ${summary.files === 1 ? "file" : "files"}${summary.directories ? ` · ${summary.directories} folders` : ""} · ${size(summary.total)}`
                : "Files, folders, or a whole collection."}
            </p>
            <div className="actions">
              <button
                id="choose"
                disabled={disabled}
                onClick={() => {
                  files.current!.value = "";
                  files.current!.click();
                }}
              >
                <FilePlus2 size={16} />
                Choose files
              </button>
              <button
                id="choose-folder"
                disabled={disabled}
                onClick={() => {
                  folder.current!.value = "";
                  folder.current!.click();
                }}
              >
                <FolderPlus size={16} />
                Choose folder
              </button>
            </div>
            <input
              ref={files}
              id="files"
              type="file"
              multiple
              hidden
              onChange={(event) => choose(fromFiles(event.target.files ?? []))}
            />
            <input
              ref={folder}
              id="folder"
              type="file"
              multiple
              hidden
              {...{ webkitdirectory: "" }}
              onChange={(event) => choose(fromFiles(event.target.files ?? []))}
            />
          </div>
          {selection ? (
            <FileQueue selection={selection} disabled={disabled} change={choose} />
          ) : (
            <div className="selection-note">
              <ShieldCheck size={17} />
              <p>
                Your files stay on these two computers.
                <br />
                No accounts or cloud storage.
              </p>
            </div>
          )}
        </section>
        {transport === "nearby" ? (
          <Devices disabled={disabled} />
        ) : (
          <RemoteDestination disabled={disabled} />
        )}
      </div>
      <div className="send-row">
        <div>
          <span className="step-number">03</span>
          <div>
            <strong>
              {destination && selection
                ? "Ready for the next step"
                : "A couple of things, then you’re off"}
            </strong>
            <p id="send-hint">
              {destination && selection
                ? `${size(summary.total)} to ${destination.name}`
                : !selection
                  ? "Select files to get started."
                  : "Choose the computer receiving your files."}
            </p>
          </div>
        </div>
        <button
          className="primary"
          id="send"
          disabled={disabled || !selection || !destination}
          onClick={() => {
            if (disabled || lock.current || !selection || !destination) return;
            lock.current = true;
            setError(null);
            send.mutate({ selected: selection, peer: destination });
          }}
        >
          {send.isPending ? "Preparing files…" : "Share files"}
          <ArrowUpRight size={18} />
        </button>
      </div>
      {(error || send.isError) && (
        <div className="error-card" role="alert">
          <span>{error ?? message(send.error)}</span>
          <button
            className="quiet"
            onClick={() => {
              setError(null);
              send.reset();
            }}
          >
            Dismiss
          </button>
        </div>
      )}
      <div className="share-explainer">
        <ShieldCheck size={16} />
        <p>Before anything is sent, you’ll compare a verification code on both computers.</p>
      </div>
    </>
  );
}
