#!/usr/bin/env python3
"""Real process/TCP tests. Python is a test tool, not an XFER dependency."""
import atexit
import hashlib
import json
import os
from pathlib import Path
import queue
import shutil
import socket
import subprocess
import sys
import tempfile
import threading

BINARY = str(Path(sys.argv[1] if len(sys.argv) > 1 else 'zig-out/bin/xfer').resolve())
TOKEN = 'integration-only-' + 'f7e628045bda981c' * 2
ENV = {**os.environ, 'XFER_TOKEN': TOKEN}
PROCESSES = []
@atexit.register
def cleanup():
    for process in PROCESSES:
        if process.poll() is None:
            process.kill()
            process.wait(timeout=10)



def free_port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]


def receiver(output, port, token=TOKEN, limit=None, discovery=False, once=True, bind='127.0.0.1'):
    args = [BINARY, '--json', 'receive', '--yes', '--output', str(output), '--bind', bind, '--port', str(port)]
    if once:
        args.append('--once')
    if not discovery:
        args.append('--no-discovery')
    if limit is not None:
        args += ['--max-bytes', str(limit)]
    process = subprocess.Popen(args, env={**ENV, 'XFER_TOKEN': token}, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    PROCESSES.append(process)
    events = []
    ready = queue.Queue()

    def read():
        for line in process.stdout:
            event = json.loads(line)
            events.append(event)
            if event['event'] == 'listening':
                ready.put(True)
        ready.put(False)

    thread = threading.Thread(target=read, daemon=True)
    thread.start()
    try:
        assert ready.get(timeout=15), events
    except BaseException:
        process.kill()
        process.wait()
        raise
    return process, events, thread


def sender(path, port, token=TOKEN, host='127.0.0.1'):
    return subprocess.run([BINARY, '--json', 'send', str(path), '--to', host, '--port', str(port), '--yes'], env={**ENV, 'XFER_TOKEN': token}, capture_output=True, text=True, timeout=60)


def finish(process, thread, expected=0):
    try:
        code = process.wait(timeout=15)
    except BaseException:
        process.kill()
        process.wait()
        raise
    thread.join(timeout=5)
    stderr = process.stderr.read()
    assert code == expected, (code, stderr)
    assert not stderr, stderr
    process.stdout.close()
    process.stderr.close()


class Proxy:
    def __init__(self, target, mode='capture'):
        self.target = target
        self.mode = mode
        self.capture = bytearray()
        self.listener = socket.socket()
        self.listener.bind(('127.0.0.1', 0))
        self.port = self.listener.getsockname()[1]
        self.listener.listen()
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    def run(self):
        with self.listener, self.listener.accept()[0] as incoming, socket.create_connection(('127.0.0.1', self.target)) as outgoing:
            def forward(source, dest, capture=False):
                count = 0
                try:
                    while data := source.recv(65536):
                        if capture:
                            self.capture.extend(data)
                            # First encrypted byte follows the 32-byte commitment + 73-byte hello and 4-byte length.
                            if self.mode == 'tamper' and count <= 109 < count + len(data):
                                data = bytearray(data)
                                data[109 - count] ^= 1
                            count += len(data)
                            if self.mode == 'interrupt' and count > 256 * 1024:
                                return
                        dest.sendall(data)
                except OSError:
                    pass
                finally:
                    for s in (source, dest):
                        try:
                            s.shutdown(socket.SHUT_RDWR)
                        except OSError:
                            pass
            reverse = threading.Thread(target=forward, args=(outgoing, incoming), daemon=True)
            reverse.start()
            forward(incoming, outgoing, True)
            reverse.join(timeout=10)

    def close(self):
        self.thread.join(timeout=15)
        assert not self.thread.is_alive()


def tree(path):
    return {str(p.relative_to(path)): ('dir' if p.is_dir() else hashlib.sha256(p.read_bytes()).hexdigest()) for p in path.rglob('*') if not p.is_symlink()}



def interactive_test(root, payload):
    # Windows lacks the Unix PTY API; native transfer tests still run there.
    if os.name == 'nt':
        return
    import pty
    import re
    import time

    class Terminal:
        def __init__(self, args):
            self.master, child = pty.openpty()
            self.output = bytearray()
            self.process = subprocess.Popen([BINARY] + args, env={**os.environ, 'XFER_TOKEN': ''}, stdin=child, stdout=child, stderr=child)
            PROCESSES.append(self.process)
            os.close(child)
            self.thread = threading.Thread(target=self.read, daemon=True)
            self.thread.start()

        def read(self):
            try:
                while data := os.read(self.master, 4096):
                    self.output.extend(data)
            except OSError:
                pass

        def wait_for(self, needle):
            deadline = time.monotonic() + 15
            while needle not in self.output.decode('utf-8', errors='replace'):
                assert self.process.poll() is None, self.output
                assert time.monotonic() < deadline, self.output
                time.sleep(0.02)
            return self.output.decode('utf-8', errors='replace')

        def approve(self, answer):
            os.write(self.master, answer.encode() + b'\n')

        def close(self):
            try:
                return self.process.wait(timeout=15)
            finally:
                if self.process.poll() is None:
                    self.process.kill()
                    self.process.wait()
                self.thread.join(timeout=3)
                os.close(self.master)

    for case in ('accept', 'decline', 'change'):
        approved = case != 'decline'
        item = payload
        if case == 'change':
            item = root / 'changing.txt'
            item.write_text('before')
        output = root / ('interactive-' + case)
        port = free_port()
        receiving = Terminal(['receive', '--once', '--no-discovery', '--bind', '127.0.0.1', '--port', str(port), '--output', str(output)])
        receiving.wait_for('Listening on')
        sending = Terminal(['send', str(item), '--to', '127.0.0.1', '--port', str(port)])
        receiver_text = receiving.wait_for('Codes match')
        sender_text = sending.wait_for('Codes match')
        code_pattern = r'Compare code ([0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4})'
        assert re.search(code_pattern, receiver_text)[1] == re.search(code_pattern, sender_text)[1]
        if case == 'change':
            item.write_text('after!')
        receiving.approve('yes' if approved else 'no')
        # The sender's affirmative answer cannot override a receiver rejection.
        sending.approve('yes')
        receiver_status = receiving.close()
        sender_status = sending.close()
        assert receiver_status == sender_status == (0 if case == 'accept' else 1), (receiving.output, sending.output)
        if case == 'accept':
            assert (output / item.name).read_bytes() == item.read_bytes()
        else:
            assert list(output.iterdir()) == []
    print('PASS real terminal consent, matching codes, recipient rejection and changing source')


def main():
    with tempfile.TemporaryDirectory(prefix='xfer-test-') as tmp:
        root = Path(tmp)
        payload = root / 'photos'
        (payload / 'nested' / 'empty').mkdir(parents=True)
        (payload / 'nested' / 'été.txt').write_text('Cross-platform Unicode contents.\n' * 100, encoding='utf-8')
        (payload / 'zero').touch()
        needle = b'PRIVATE-FILE-CONTENT-MUST-BE-ENCRYPTED-51c486'
        (payload / 'large.bin').write_bytes(needle * 1000 + os.urandom(5 * 1024 * 1024))
        # Symlinks are never copied or followed; skip only if the host forbids creation.
        try:
            (payload / 'outside-link').symlink_to(root / 'outside.txt')
        except OSError:
            pass
        destination = root / 'received'
        port = free_port()
        r, events, t = receiver(destination, port)
        proxy = Proxy(port)
        s = sender(payload, proxy.port)
        assert s.returncode == 0, (s.stdout, s.stderr)
        finish(r, t)
        proxy.close()
        assert needle not in proxy.capture
        assert any(e['event'] == 'received' for e in events)
        expected = tree(payload)
        expected.pop('outside-link', None)
        assert tree(destination / 'photos') == expected
        assert not list(destination.glob('.xfer-*.part'))
        print('PASS encrypted folder transfer, Unicode, empty files/directories, symlink skip and verified acknowledgement')

        original = tree(destination / 'photos')
        port = free_port()
        r, events, t = receiver(destination, port)
        s = sender(payload, port)
        assert s.returncode == 0, s.stdout
        finish(r, t)
        assert tree(destination / 'photos') == original
        assert tree(destination / 'photos (1)') == original
        print('PASS collision-safe publication preserves the original')

        for mode in ('tamper', 'interrupt'):
            output = root / mode
            port = free_port()
            r, events, t = receiver(output, port)
            proxy = Proxy(port, mode)
            s = sender(payload, proxy.port)
            assert s.returncode != 0
            finish(r, t, 1)
            proxy.close()
            assert list(output.iterdir()) == [], list(output.iterdir())
            print(f'PASS {mode} rejects the session and removes staging')

        for case, token, limit in [('wrong-secret', TOKEN + '-wrong', None), ('size-limit', TOKEN, 1)]:
            output = root / case
            port = free_port()
            r, events, t = receiver(output, port, token, limit)
            s = sender(payload, port)
            assert s.returncode != 0, s.stdout
            finish(r, t, 1)
            assert list(output.iterdir()) == []
            print(f'PASS {case} prevents publication')

        port = free_port()
        output = root / 'discovery'
        r, events, t = receiver(output, port, discovery=True)
        peers = subprocess.run([BINARY, '--json', 'discover', '--port', str(port)], capture_output=True, text=True, timeout=10)
        assert peers.returncode == 0 and any(json.loads(line)['event'] == 'peer' for line in peers.stdout.splitlines()), peers
        s = sender(payload / 'zero', port)
        assert s.returncode == 0, s.stdout
        finish(r, t)
        assert (output / 'zero').read_bytes() == b''
        print('PASS nearby discovery and standalone empty-file transfer')

        try:
            with socket.socket(socket.AF_INET6) as v6:
                v6.bind(('::1', 0))
                port = v6.getsockname()[1]
        except OSError:
            print('SKIP IPv6: host has no IPv6 loopback')
        else:
            output = root / 'ipv6'
            r, events, t = receiver(output, port, bind='::1')
            s = sender(payload / 'zero', port, host='[::1]:' + str(port))
            assert s.returncode == 0, (s.stdout, s.stderr)
            finish(r, t)
            assert (output / 'zero').read_bytes() == b''
            print('PASS direct IPv6 transfer with bracketed endpoint')

        # The persistent listener must recover from a failed session.
        port = free_port()
        output = root / 'persistent'
        r, events, t = receiver(output, port, once=False)
        try:
            assert sender(payload / 'zero', port, TOKEN + '-wrong').returncode != 0
            assert sender(payload / 'zero', port).returncode == 0
            assert (output / 'zero').is_file()
        finally:
            r.terminate()
            r.wait(timeout=10)
            t.join(timeout=5)
            r.stdout.close()
            r.stderr.close()
        print('PASS persistent receiver recovers after failed authentication')

        dry = subprocess.run([BINARY, '--json', 'send', str(payload), '--dry-run'], capture_output=True, text=True, timeout=15)
        assert dry.returncode == 0 and 'planned' in dry.stdout, dry.stdout
        insecure = subprocess.run([BINARY, 'receive', '--yes'], env={**os.environ, 'XFER_TOKEN': ''}, capture_output=True, text=True, timeout=5)
        assert insecure.returncode != 0 and 'SharedSecretRequired' in insecure.stderr
        print('PASS offline preview and unattended approval requires a secret')
        interactive_test(root, payload / 'zero')
    print('All integration tests passed.')


if __name__ == '__main__':
    main()
