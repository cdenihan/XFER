'use strict';
// Execute the actual browser script with minimal DOM/fetch adapters.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const { QueryClient, QueryObserver } = require('../web/node_modules/@tanstack/query-core');
const elements = new Map();
function node() {
  return { style: {}, classList: { add() {}, remove() {}, toggle() {} },
    append() {}, replaceChildren() {}, setAttribute() {}, addEventListener() {},
    close() { this.open = false; }, showModal() { this.open = true; } };
}
const calls = [];
let failState = false;
let failNew = false;
let failUpload = false;
let failDecision = false;
let decisionPending;
const state = { name: 'Test', output: '/tmp', phase: 'ready', busy: false, peers: [], pending: null, total: 0, bytes: 0 };
const context = vm.createContext({
  document: { getElementById(id) { if (!elements.has(id)) elements.set(id, node()); return elements.get(id); }, createElement: node },
  location: { hash: '#capability', pathname: '/' }, sessionStorage: { getItem() { return ''; }, setItem() {} }, history: { replaceState() {} }, File: class File {},
  QueryClient, QueryObserver, setTimeout, clearTimeout, setInterval, clearInterval,
  async fetch(url) {
    calls.push(url);
    if (url === '/api/state' && failState) throw new Error('Failed to fetch');
    if (url === '/api/new' && failNew) return { ok: false, async json() { return { error: 'TransferInProgress' }; } };
    if (url === '/api/upload' && failUpload) throw new Error('Failed to fetch');
    if (url === '/api/decision' && failDecision) {
      if (decisionPending !== undefined) state.pending = decisionPending;
      throw new Error('Failed to fetch');
    }
    return { ok: true, async json() { return url === '/api/state' ? state : {}; } };
  }
});
const evaluate = (source) => vm.runInContext(source, context);
(async () => {
  const source = fs.readFileSync(require('node:path').join(__dirname, '../web/src/app.ts'), 'utf8').replace(/^import .*;$/gm, '');
  evaluate(new Bun.Transpiler({ loader: 'ts' }).transformSync(source));
  await new Promise(setImmediate);
  evaluate("selection = {root: 'file', folder: false, items: [{path: 'file', file: new File()}]}; destination = {address: 'localhost', name: 'peer'};");
  calls.length = 0;
  failNew = true;
  await elements.get('send').onclick();
  assert(!calls.includes('/api/cancel'), 'Rejected new selection must not cancel incoming work');
  failNew = false;
  failUpload = true;
  await elements.get('send').onclick();
  assert(calls.includes('/api/cancel'), 'An owned interrupted upload must be cleaned up');
  failState = true;
  await evaluate('observer.refetch()');
  assert.equal(elements.get('online').textContent, 'Reconnecting…');
  failState = false;
  await evaluate('observer.refetch()');
  assert.equal(elements.get('online').textContent, 'Ready to receive');
  assert.equal(evaluate('disconnected'), false);
  state.pending = { id: 1, code: '123456789abc', receiving: true, name: 'file', total: 1 };
  await evaluate('observer.refetch()');
  elements.get('match').checked = true;
  elements.get('match').onchange();
  failDecision = true;
  await elements.get('accept').onclick();
  assert.equal(elements.get('accept').disabled, false, 'Failed acceptance must be retryable when codes match');
  assert.equal(elements.get('decline').disabled, false);
  assert.equal(elements.get('approval').open, true);
  elements.get('match').checked = false;
  await elements.get('decline').onclick();
  assert.equal(elements.get('accept').disabled, true, 'Unconfirmed codes must still block acceptance');
  elements.get('match').checked = true;
  decisionPending = { ...state.pending, id: 2 };
  await elements.get('accept').onclick();
  assert.equal(elements.get('accept').disabled, true, 'A stale failure must not enable approval for a different request');
  await evaluate('observer.refetch()');
  elements.get('match').checked = true;
  elements.get('match').onchange();
  decisionPending = null;
  await elements.get('accept').onclick();
  assert.equal(elements.get('accept').disabled, true, 'An expired request must not be re-enabled');
  decisionPending = undefined;
  state.pending = { id: 3, code: '123456789abc', receiving: true, name: 'file', total: 1 };
  await evaluate('observer.refetch()');
  elements.get('match').checked = true;
  failDecision = false;
  await elements.get('accept').onclick();
  assert.equal(elements.get('approval').open, false, 'Successful acceptance must close the dialog');
  await elements.get('quit').onclick();
  assert.equal(evaluate('observer.hasListeners()'), false, 'Explicit quit must unsubscribe polling');
  assert.equal(evaluate('queryClient.getQueryCache().getAll().length'), 0, 'Explicit quit must remove cached state');
  evaluate('unsubscribe(); queryClient.clear()');
  console.log('PASS selection ownership, transient poll recovery, approval retry and explicit quit');
})().catch(error => { console.error(error); process.exitCode = 1; });
