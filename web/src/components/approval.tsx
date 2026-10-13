import { useEffect, useRef, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { ShieldCheck } from "../icons";
import { api, useLive, useSession, selectPending } from "../session";
import { message } from "../api";
import { size } from "../selection";
import type { Pending } from "../types";

function ApprovalDialog({ pending }: { pending: Pending }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [matches, setMatches] = useState(false);
  const [answered, setAnswered] = useState(false);
  const client = useQueryClient();
  const decision = useMutation({
    mutationFn: (approve: boolean) =>
      api("decision", { id: pending.id, approve, code: pending.code }),
    onSuccess: () => {
      setAnswered(true);
      dialog.current?.close();
      void client.invalidateQueries({ queryKey: ["state"] });
    },
    onError: () => {
      void client.invalidateQueries({ queryKey: ["state"] });
    },
  });
  useEffect(() => {
    const node = dialog.current;
    node?.showModal();
    return () => node?.close();
  }, []);
  return (
    <dialog
      ref={dialog}
      id="approval"
      aria-labelledby="approval-title"
      onCancel={(event) => {
        event.preventDefault();
        if (!decision.isPending && !answered) decision.mutate(false);
      }}
    >
      <div className="dialog-icon">
        <ShieldCheck size={28} />
      </div>
      <p className="eyebrow">ONE LAST CHECK</p>
      <h2 id="approval-title">
        {pending.receiving ? "Someone wants to share." : "Confirm your connection."}
      </h2>
      <p className="approval-direction">
        {pending.receiving ? "INCOMING TRANSFER" : "OUTGOING TRANSFER"}
      </p>
      <p id="approval-detail">
        {pending.name} · {size(pending.total)}
      </p>
      <p className="code-label">Verification code</p>
      <div className="code" id="code">
        {pending.code.match(/.{1,4}/g)?.join("-")}
      </div>
      <p>
        Compare the <strong>entire code</strong> on both computers. Continue only if they match.
      </p>
      <label className="confirm">
        <input
          id="match"
          type="checkbox"
          checked={matches}
          disabled={decision.isPending || answered}
          onChange={(event) => setMatches(event.target.checked)}
        />
        I checked that the codes match on both computers.
      </label>
      {decision.isError && (
        <p className="error" role="alert">
          {message(decision.error)}
        </p>
      )}
      <div className="dialog-actions">
        <button
          id="decline"
          disabled={decision.isPending || answered}
          onClick={() => decision.mutate(false)}
        >
          Decline
        </button>
        <button
          className="primary"
          id="accept"
          disabled={!matches || decision.isPending || answered}
          onClick={() => decision.mutate(true)}
        >
          {decision.isPending ? "Confirming…" : pending.receiving ? "Accept files" : "Share files"}
        </button>
      </div>
    </dialog>
  );
}
export function Approval() {
  const state = useLive(selectPending);
  const { closed } = useSession();
  const pending = state.isError || closed ? null : state.data;
  return pending ? (
    <ApprovalDialog key={`${pending.id}:${pending.code}`} pending={pending} />
  ) : null;
}
