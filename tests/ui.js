'use strict';
// Execute the actual browser script with minimal DOM/fetch adapters.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const elements = new Map();
function node() {
  return { style: {}, classList: { add() {}, remove() {}, toggle() {} },
    append() {}, replaceChildren() {}, setAttribute() {}, addEventListener() {},
    close() { this.open = false; }, showModal() { this.open = true; } };
}
const calls = [];
const timers = [];
let failState = false;
let failNew = false;
let failUpload = false;
const state = { name: 'Test', output: '/tmp', phase: 'ready', busy: false, peers: [], pending: null, total: 0, bytes: 0 };
const context = vm.createContext({
  document: { getElementById(id) { if (!elements.has(id)) elements.set(id, node()); return elements.get(id); }, createElement: node },
  location: { hash: '#capability', pathname: '/' }, sessionStorage: { getItem() { return ''; }, setItem() {} }, history: { replaceState() {} }, File: class File {},
  setTimeout(fn) { timers.push(fn); },
  async fetch(url) {
    calls.push(url);
    if (url === '/api/state' && failState) throw new Error('Failed to fetch');
    if (url === '/api/new' && failNew) return { ok: false, async json() { return { error: 'TransferInProgress' }; } };
    if (url === '/api/upload' && failUpload) throw new Error('Failed to fetch');
    return { ok: true, async json() { return url === '/api/state' ? state : {}; } };
  }
});
const evaluate = (source) => vm.runInContext(source, context);
(async () => {
  evaluate(fs.readFileSync('src/ui/app.js', 'utf8'));
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
  await evaluate('poll()');
  assert.equal(elements.get('online').textContent, 'Reconnecting…');
  failState = false;
  await timers.pop()();
  assert.equal(elements.get('online').textContent, 'Ready to receive');
  assert.equal(evaluate('disconnected'), false);
  await elements.get('quit').onclick();
  calls.length = 0;
  await evaluate('poll()');
  assert.equal(calls.length, 0, 'Explicit quit must stop polling');
  console.log('PASS selection ownership, transient poll recovery and explicit quit');
})().catch(error => { console.error(error); process.exitCode = 1; });
