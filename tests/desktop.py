#!/usr/bin/env python3
"""Exercise the loopback browser API against real encrypted transfer processes."""
import hashlib
import http.client
import json
import os
from pathlib import Path
import queue
import socket
import subprocess
import sys
import tempfile
import threading
import time
from urllib.parse import quote, urlsplit

BINARY = str(Path(sys.argv[1]).resolve())


def port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]


class Desktop:
    def __init__(self, root, name, limit=16 * 1024**3, occupied_udp=False):
        self.root = root
        self.root.mkdir()
        self.port = port()
        self.output = root / 'received'
        occupied = socket.socket(socket.AF_INET, socket.SOCK_DGRAM) if occupied_udp else None
        if occupied:
            occupied.bind(('127.0.0.1', self.port))
        env = {k: v for k, v in os.environ.items() if k != 'XFER_TOKEN'}
        env.update(TMPDIR=str(root), TEMP=str(root), TMP=str(root))
        self.process = subprocess.Popen([BINARY, '--no-open', '--json', '--bind', '127.0.0.1', '--port', str(self.port), '--name', name, '--output', str(self.output), '--max-bytes', str(limit)], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.events = queue.Queue()
        def consume():
            for line in self.process.stdout:
                self.events.put(json.loads(line))
        threading.Thread(target=consume, daemon=True).start()
        event = self.events.get(timeout=10)
        assert event['event'] == 'desktop', event
        if occupied:
            occupied.close()
        url = urlsplit(event['message'])
        self.host, self.http_port, self.token = url.hostname, url.port, url.fragment

    def request(self, path, body=None, method='POST', headers=None, status=200):
        connection = http.client.HTTPConnection(self.host, self.http_port, timeout=10)
        payload = body if isinstance(body, bytes) else json.dumps(body).encode() if body is not None else None
        request_headers = {'Authorization': 'Bearer ' + self.token, 'Content-Type': 'application/json'}
        request_headers.update(headers or {})
        try:
            connection.request(method, path, payload, request_headers)
            response = connection.getresponse()
            data = response.read()
            assert response.status == status, (path, response.status, data)
            return json.loads(data) if path.startswith('/api/') else (data, response.getheaders())
        finally:
            connection.close()

    def state(self):
        return self.request('/api/state', method='GET')

    def wait(self, predicate, seconds=15):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            state = self.state()
            if predicate(state):
                return state
            time.sleep(.05)
        raise AssertionError('Timed out: ' + json.dumps(self.state()))

    def approve(self, pending, approve=True):
        self.request('/api/decision', {'id': pending['id'], 'code': pending['code'], 'approve': approve})

    def prepare(self, name, files, folders=()):
        self.request('/api/new', {'name': name, 'folder': bool(folders) or len(files) != 1 or files[0][0] != name})
        for path in folders:
            self.request('/api/directory', {'path': path})
        for path, content in files:
            self.request('/api/upload', content, method='PUT', headers={'X-Xfer-Path': quote(path, safe='')})

    def close(self):
        if self.process.poll() is None:
            self.request('/api/quit', {})
        self.process.wait(timeout=10)
        stderr = self.process.stderr.read()
        assert self.process.returncode == 0, stderr
        assert not list(self.root.glob('xfer-upload-*')), 'Private browser uploads leaked on exit'
        assert not list(self.output.glob('.xfer-*.part')), 'Transfer staging leaked on exit'


def run():
    with tempfile.TemporaryDirectory(prefix='xfer-desktop-test-') as directory:
        root = Path(directory)
        sender = Desktop(root / 'sender', 'Sender', occupied_udp=True)
        receiver = Desktop(root / 'receiver', 'Receiver')
        try:
            page, headers = sender.request('/', method='GET')
            assert b'Drop files or a folder here' in page
            assert b'webkitdirectory' in page
            assert "frame-ancestors 'none'" in dict(headers)['Content-Security-Policy']
            sender.request('/api/state', method='GET', headers={'Authorization': 'Bearer wrong'}, status=401)
            sender.request('/api/state', method='GET', headers={'Origin': 'https://malicious.example'}, status=403)
            sender.request('/api/state', method='GET', headers={'Host': 'malicious.example'}, status=403)
            sender.request('/api/new', {'name': '../escape'}, status=400)
            sender.request('/api/new', {'name': 'CON'}, status=400)
            sender.request('/api/new', {'name': 'valid', 'folder': True})
            sender.request('/api/upload', b'bad', method='PUT', headers={'X-Xfer-Path': 'valid%2F%2E%2E%2Fescape'}, status=400)
            sender.request('/api/upload', b'bad', method='PUT', headers={'X-Xfer-Path': 'other/file'}, status=400)
            sender.request('/api/cancel', {})
            assert not sender.state()['busy']
            assert not list(sender.root.glob('xfer-upload-*'))
            sender.request('/api/new', {'name': 'partial.bin'})
            partial = socket.create_connection((sender.host, sender.http_port), timeout=5)
            header = f'PUT /api/upload HTTP/1.1\r\nHost: {sender.host}:{sender.http_port}\r\nAuthorization: Bearer {sender.token}\r\nX-Xfer-Path: partial.bin\r\nContent-Length: 1048576\r\n\r\n'
            partial.sendall(header.encode() + b'partial')
            time.sleep(.1)
            # The upload body is blocked; the state API and cancel must still work.
            assert sender.state()['busy']
            sender.request('/api/cancel', {})
            sender.wait(lambda s: not s['busy'], seconds=3)
            partial.close()
            assert not list(sender.root.glob('xfer-upload-*'))
            print('PASS browser capability, origin/host protection, traversal and blocked-upload cancellation')

            idle = socket.create_connection(('127.0.0.1', receiver.port), timeout=5)
            receiver.wait(lambda s: s['busy'])
            receiver.request('/api/cancel', {})
            receiver.wait(lambda s: not s['busy'], seconds=3)
            idle.close()
            assert not list(receiver.output.glob('.xfer-*.part'))
            print('PASS browser cancellation wakes a stalled incoming handshake')

            content = os.urandom(2 * 1024 * 1024 + 97)
            sender.prepare('été', [('été/nested/photo.bin', content), ('été/empty.txt', b'')], ['été', 'été/empty-directory'])
            sender.request('/api/send', {'to': 'localhost:' + str(receiver.port)})
            left = sender.wait(lambda s: s['pending'] is not None)['pending']
            right = receiver.wait(lambda s: s['pending'] is not None)['pending']
            assert left['code'] == right['code'] and not left['receiving'] and right['receiving']
            receiver.request('/api/decision', {'id': right['id'] + 1, 'code': right['code'], 'approve': True}, status=400)
            assert not (receiver.output / 'été').exists(), 'Files published before consent'
            sender.approve(left)
            receiver.approve(right)
            sender.wait(lambda s: not s['busy'] and s['phase'] == 'sent')
            receiver.wait(lambda s: not s['busy'] and s['phase'] == 'received')
            assert hashlib.sha256((receiver.output / 'été/nested/photo.bin').read_bytes()).digest() == hashlib.sha256(content).digest()
            assert (receiver.output / 'été/empty-directory').is_dir()
            assert (receiver.output / 'été/empty.txt').read_bytes() == b''
            print('PASS manual hostname transfer despite occupied discovery port, matching consent, UTF-8 and empty folders')

            sender.prepare('declined.txt', [('declined.txt', b'must not arrive')])
            sender.request('/api/send', {'to': '127.0.0.1:' + str(receiver.port)})
            left = sender.wait(lambda s: s['pending'] is not None)['pending']
            right = receiver.wait(lambda s: s['pending'] is not None)['pending']
            receiver.approve(right, False)
            sender.approve(left)
            sender.wait(lambda s: not s['busy'])
            receiver.wait(lambda s: not s['busy'])
            assert not (receiver.output / 'declined.txt').exists()
            print('PASS browser rejection preserves destination and receiver recovers')
        finally:
            sender.close()
            receiver.close()
        limited = Desktop(root / 'limited', 'Limited', limit=4)
        try:
            limited.request('/api/new', {'name': 'big.txt'})
            limited.request('/api/upload', b'12345', method='PUT', headers={'X-Xfer-Path': 'big.txt'}, status=400)
            limited.request('/api/cancel', {})
            print('PASS browser upload limit')
        finally:
            limited.close()


if __name__ == '__main__':
    run()
