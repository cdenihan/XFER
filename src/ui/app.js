'use strict';

const element = (id) => document.getElementById(id);
const token = location.hash.slice(1) || sessionStorage.getItem('xfer-capability') || '';
if (token) {
  sessionStorage.setItem('xfer-capability', token);
  history.replaceState(null, '', location.pathname);
}

let selection = null;
let destination = null;
let current = null;
let preparing = false;
let pendingId = null;
let disconnected = false;
let pollFailed = false;
let localError = null;
let renderedPeers = null;

const errors = {
  Timeout: 'The other computer did not respond in time. Check its firewall and try again.',
  ConnectionRefused: 'The other computer is not receiving. Open XFER there and try again.',
  Declined: 'The transfer was declined.',
  PeerRejected: 'The transfer was declined.',
  TransferRejected: 'The transfer was declined.',
  AuthenticationFailed: 'The secure connection could not be verified.',
  TransferInProgress: 'Another transfer is in progress.',
  InvalidName: 'A filename cannot be used on all three platforms. Rename it and try again.',
  InvalidPath: 'A filename cannot be used on all three platforms. Rename it and try again.',
  InvalidUpload: 'These files exceed the limit or have an invalid path.',
  RequestTooLarge: 'The selection exceeds the transfer limit.',
  TransferTooLarge: 'The selection exceeds the transfer limit.',
  Canceled: 'Transfer canceled.',
  StaleApproval: 'This request has expired.',
};
const failure = (error) => errors[error] || 'The transfer could not finish. Check the other computer and try again.';

function size(bytes) {
  if (bytes < 1024) return bytes + ' B';
  const units = ['KiB', 'MiB', 'GiB', 'TiB'];
  let index = -1;
  do {
    bytes /= 1024;
    index++;
  } while (bytes >= 1024 && index < units.length - 1);
  return bytes.toFixed(bytes >= 10 ? 0 : 1) + ' ' + units[index];
}

async function api(path, body, method = 'POST', extraHeaders = {}) {
  const file = body instanceof File;
  const response = await fetch('/api/' + path, {
    method,
    headers: {
      Authorization: 'Bearer ' + token,
      ...(file ? {} : { 'Content-Type': 'application/json' }),
      ...extraHeaders,
    },
    body: body === undefined ? undefined : file ? body : JSON.stringify(body),
  });
  const result = await response.json();
  if (!response.ok) throw new Error(failure(result.error));
  return result;
}

function notice(message) {
  localError = message;
  element('status').hidden = false;
  element('status-text').textContent = message;
  element('status-text').classList.add('failed');
  element('progress').hidden = true;
  element('stats').textContent = '';
}

function updateControls() {
  const busy = preparing || current?.busy || disconnected || pollFailed;
  for (const id of ['choose', 'choose-folder', 'clear']) element(id).disabled = busy;
  element('send').disabled = busy || !selection || !destination;
  element('send-hint').textContent = destination
    ? 'Share with ' + destination.name
    : 'Choose files and a receiving computer.';
}

function select(items, root, folder) {
  if (preparing || current?.busy) return;
  localError = null;
  selection = items.length || folder ? { items, root, folder } : null;
  element('selection').textContent = selection ? root : 'Drop files or a folder here';
  const files = items.filter((item) => item.file).length;
  const bytes = items.reduce((total, item) => total + (item.file?.size || 0), 0);
  element('selection-detail').textContent = selection
    ? files + (files === 1 ? ' file · ' : ' files · ') + size(bytes)
    : 'Your files stay between these two computers.';
  element('clear').hidden = !selection;
  updateControls();
}

function fromFiles(files) {
  const items = Array.from(files);
  if (!items.length) return select([], null, false);
  if (items[0].webkitRelativePath) {
    select(items.map((file) => ({ file, path: file.webkitRelativePath })),
      items[0].webkitRelativePath.split('/')[0], true);
  } else if (items.length === 1) {
    select([{ file: items[0], path: items[0].name }], items[0].name, false);
  } else {
    select(items.map((file) => ({ file, path: 'Shared items/' + file.name })), 'Shared items', true);
  }
}

for (const [button, input] of [['choose', 'files'], ['choose-folder', 'folder']]) {
  element(button).onclick = () => {
    element(input).value = '';
    element(input).click();
  };
  element(input).onchange = (event) => fromFiles(event.target.files);
}
element('clear').onclick = () => select([], null, false);

// Directory readers return batches; read until exhausted and retain empty folders.
async function walk(entry, prefix = '') {
  const path = prefix + entry.name;
  if (entry.isFile) {
    const file = await new Promise((resolve, reject) => entry.file(resolve, reject));
    return [{ file, path }];
  }
  if (!entry.isDirectory) return [];
  const items = [{ path, directory: true }];
  const reader = entry.createReader();
  while (true) {
    const entries = await new Promise((resolve, reject) => reader.readEntries(resolve, reject));
    if (!entries.length) break;
    for (const child of entries) items.push(...await walk(child, path + '/'));
  }
  return items;
}

element('drop').ondragover = (event) => {
  event.preventDefault();
  if (!current?.busy) element('drop').classList.add('over');
};
element('drop').ondragleave = () => element('drop').classList.remove('over');
element('drop').ondrop = async (event) => {
  event.preventDefault();
  element('drop').classList.remove('over');
  if (preparing || current?.busy) return;
  try {
    const entries = Array.from(event.dataTransfer.items || [])
      .map((item) => item.webkitGetAsEntry?.()).filter(Boolean);
    if (!entries.length) return fromFiles(event.dataTransfer.files);
    let items = [];
    for (const entry of entries) items.push(...await walk(entry));
    if (entries.length === 1) {
      select(items, entries[0].name, entries[0].isDirectory);
    } else {
      select(items.map((item) => ({ ...item, path: 'Shared items/' + item.path })), 'Shared items', true);
    }
  } catch {
    notice('These files could not be read. Try choosing them with the file picker.');
  }
};

const computerIcon = '<svg width="25" height="25" viewBox="0 0 24 24" fill="none" aria-hidden="true"><rect x="3" y="4" width="18" height="12" rx="2" stroke="currentColor" stroke-width="1.7"/><path d="M8 20h8m-4-4v4" stroke="currentColor" stroke-width="1.7" stroke-linecap="round"/></svg>';
function showPeers(peers) {
  const key = JSON.stringify([peers, destination?.address]);
  if (key === renderedPeers) return;
  renderedPeers = key;
  const container = element('peers');
  container.replaceChildren();
  if (!peers.length) {
    const empty = document.createElement('div');
    empty.className = 'empty';
    const title = document.createElement('strong');
    title.textContent = 'Waiting for a nearby computer';
    empty.append(title, 'Open XFER on another computer on the same network.');
    container.append(empty);
  }
  for (const peer of peers) {
    const selected = destination?.address === peer.address;
    const button = document.createElement('button');
    button.className = 'peer' + (selected ? ' selected' : '');
    button.setAttribute('aria-pressed', String(selected));
    const icon = document.createElement('span');
    icon.className = 'avatar';
    icon.innerHTML = computerIcon; // Static markup; peer text never enters HTML.
    const name = document.createElement('strong');
    name.textContent = peer.name;
    const address = document.createElement('small');
    address.textContent = peer.address;
    button.append(icon, name, address);
    button.onclick = () => {
      destination = peer;
      showPeers(current.peers);
      updateControls();
    };
    container.append(button);
  }
  element('scanning').textContent = peers.length ? peers.length + ' available' : 'Looking nearby…';
}

element('connect').onclick = () => {
  const address = element('address').value.trim();
  if (!address) return;
  destination = { address, name: address };
  showPeers(current?.peers || []);
  updateControls();
};

element('send').onclick = async () => {
  if (!selection || !destination) return;
  preparing = true;
  localError = null;
  updateControls();
  let uploadCreated = false;
  try {
    await api('new', { name: selection.root, folder: selection.folder, total: selection.items.reduce((bytes, item) => bytes + (item.file?.size || 0), 0) });
    uploadCreated = true;
    for (const item of selection.items) {
      if (item.directory) await api('directory', { path: item.path });
      else await api('upload', item.file, 'PUT', { 'X-Xfer-Path': encodeURIComponent(item.path) });
    }
    await api('send', { to: destination.address });
    uploadCreated = false;
    selection = null;
    element('selection').textContent = 'Drop files or a folder here';
    element('selection-detail').textContent = 'Your files stay between these two computers.';
    element('clear').hidden = true;
  } catch (error) {
    if (uploadCreated) {
      try { await api('cancel', {}); } catch { }
    }
    notice(error.message === 'Failed to fetch' ? 'File preparation was interrupted. Select the files and try again.' : error.message);
  } finally {
    preparing = false;
    updateControls();
  }
};

element('cancel').onclick = async () => {
  try { await api('cancel', {}); } catch (error) { notice(error.message); }
};
element('match').onchange = () => { element('accept').disabled = !element('match').checked; };

async function decide(approve) {
  if (!current?.pending) return;
  const pending = current.pending;
  element('accept').disabled = element('decline').disabled = true;
  try {
    await api('decision', { id: pending.id, approve, code: pending.code });
    element('approval').close();
  } catch (error) {
    if (current?.pending?.id === pending.id && current.pending.code === pending.code) {
      element('accept').disabled = !element('match').checked;
    }
    notice(error.message);
  } finally {
    element('decline').disabled = false;
  }
}
element('accept').onclick = () => decide(true);
element('decline').onclick = () => decide(false);
element('approval').addEventListener('cancel', (event) => {
  event.preventDefault();
  decide(false);
});

element('quit').onclick = async () => {
  try {
    await api('quit', {});
    disconnected = true;
    element('online').textContent = 'XFER is closed';
    element('status').hidden = false;
    element('status-text').textContent = 'You can close this window. Launch XFER to share again.';
    element('cancel').hidden = element('progress').hidden = true;
    updateControls();
  } catch (error) {
    notice(error.message);
  }
};

function showStatus(state) {
  if (localError) return notice(localError);
  if (state.phase === 'ready') return;
  element('status').hidden = false;
  element('status-text').textContent = state.phase === 'failed' ? failure(state.message)
    : state.phase === 'sent' ? 'Sent ' + state.message
      : state.phase === 'received' ? 'Saved ' + state.message
        : state.message;
  element('status-text').classList.toggle('failed', state.phase === 'failed');
  element('cancel').hidden = !state.busy;
  const progress = state.total > 0 && ['progress', 'sent', 'received', 'uploading'].includes(state.phase);
  element('progress').hidden = !progress;
  element('bar').style.width = Math.min(100, state.bytes / state.total * 100) + '%';
  element('stats').textContent = progress ? size(state.bytes) + ' of ' + size(state.total) : '';
}

function showApproval(pending) {
  if (!pending) {
    pendingId = null;
    if (element('approval').open) element('approval').close();
    return;
  }
  if (pendingId === pending.id) return;
  pendingId = pending.id;
  element('match').checked = false;
  element('accept').disabled = true;
  element('approval-title').textContent = pending.receiving ? 'Accept these files?' : 'Ready to share?';
  element('approval-detail').textContent = pending.name + ' · ' + size(pending.total);
  element('code').textContent = pending.code.slice(0, 4) + '-' + pending.code.slice(4, 8) + '-' + pending.code.slice(8);
  element('accept').textContent = pending.receiving ? 'Accept' : 'Share';
  if (!element('approval').open) element('approval').showModal();
}

async function poll() {
  if (disconnected) return;
  try {
    current = await api('state', undefined, 'GET');
    if (pollFailed) { localError = null; pollFailed = false; element('status').hidden = true; }
    element('identity').textContent = current.name;
    element('destination').textContent = 'Saved to ' + current.output;
    element('online').textContent = current.busy ? 'Sharing in progress' : 'Ready to receive';
    showPeers(current.peers);
    showStatus(current);
    showApproval(current.pending);
    updateControls();
  } catch {
    pollFailed = true;
    element('online').textContent = 'Reconnecting…';
    notice('Connection interrupted. Retrying…');
    updateControls();
  }
  setTimeout(poll, 600);
}
poll();
