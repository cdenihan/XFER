import { ArrowDownLeft, Folder, ShieldCheck, Wifi, Check, Monitor } from "../icons";
import { RemoteReceiver } from "./transport";
import { useSession, useLive, selectReceiving } from "../session";
export function Receive() {
  const state = useLive(selectReceiving);
  const { closed } = useSession();
  const offline = closed || state.isError || !state.data;
  return (
    <>
      <div className="page-heading">
        <div>
          <p className="eyebrow">THIS COMPUTER IS YOUR DESTINATION</p>
          <h1>Welcome something new.</h1>
          <p>Keep this window open. You decide what comes in.</p>
        </div>
        <span className="heading-badge">
          <ShieldCheck size={15} />
          Always with your permission
        </span>
      </div>
      <RemoteReceiver />
      <div className="receive-layout">
        <section className="panel receive-card">
          <div className={`receive-orbit ${offline ? "offline" : ""}`}>
            <ArrowDownLeft size={40} strokeWidth={1.4} />
          </div>
          <span className={`pill ${offline ? "offline" : ""}`}>
            <i className="dot" />
            {closed
              ? "XFER is closed"
              : offline
                ? "Waiting for connection"
                : state.data?.busy
                  ? "Transfer in progress"
                  : "Ready to receive"}
          </span>
          <h2>{state.data?.name ?? "This computer"}</h2>
          <p>This is the name other computers see.</p>
          <div className="receive-availability">
            <Monitor size={17} />
            {offline
              ? "Launch XFER to make this computer available."
              : "Visible to computers on your local network"}
          </div>
        </section>
        <div className="receive-information">
          <section className="panel save-location">
            <span className="card-icon">
              <Folder size={22} />
            </span>
            <p className="eyebrow">YOUR RECEIVED FILES</p>
            <h2>Everything lands here.</h2>
            <p className="output-path" id="destination">
              {state.data?.output ?? "Downloads / XFER"}
            </p>
            <p>Existing files are preserved. New items get their own names.</p>
          </section>
          <section className="panel receive-guide">
            <h2>A safe arrival, every time.</h2>
            <ol>
              <li>
                <span>
                  <Wifi size={17} />
                </span>
                <div>
                  <strong>Find each other</strong>
                  <p>Open XFER on both computers, on the same Wi-Fi or Ethernet network.</p>
                </div>
              </li>
              <li>
                <span>
                  <ShieldCheck size={17} />
                </span>
                <div>
                  <strong>Compare the code</strong>
                  <p>Check the entire code on both screens. Accept only if they match.</p>
                </div>
              </li>
              <li>
                <span>
                  <Check size={17} />
                </span>
                <div>
                  <strong>Wait for delivery</strong>
                  <p>Files appear in your destination folder after verification completes.</p>
                </div>
              </li>
            </ol>
          </section>
        </div>
      </div>
      <p className="receive-footnote">
        Closing the browser tab leaves XFER running. Use Quit XFER to stop receiving.
      </p>
    </>
  );
}
