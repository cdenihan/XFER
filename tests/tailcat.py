#!/usr/bin/env python3
"""Real optional helper integration, using Tailcat's isolated loopback DERP mode."""
import os
from pathlib import Path
import tempfile
import desktop

def run():
    os.environ['TS_DEBUG_TAILCAT_LOCAL_DERP'] = '1'
    os.environ['TAILCAT_DERPMAP_URL'] = 'none'
    with tempfile.TemporaryDirectory(prefix='xfer-tailcat-test-') as directory:
        root = Path(directory)
        receiver = desktop.Desktop(root / 'receiver', 'Remote receiver')
        sender = desktop.Desktop(root / 'sender', 'Remote sender')
        try:
            assert receiver.state()['tailcat']['available'], 'Install Tailcat 0.7+ or set XFER_TAILCAT_BIN'
            receiver.request('/api/tailcat/start', {})
            invite = receiver.wait(lambda s: s['tailcat']['enabled'])['tailcat']['invite']
            receiver.request('/api/tailcat/start', {}, status=400)
            receiver.request('/api/tailcat/start', {}, headers={'Authorization': 'Bearer wrong'}, status=401)
            sender.prepare('remote.txt', [('remote.txt', b'verified remote bytes')])
            sender.request('/api/send', {'to': invite})
            a = sender.wait(lambda s: s['pending'])['pending']
            b = receiver.wait(lambda s: s['pending'])['pending']
            assert a['code'] == b['code']
            sender.approve(a); receiver.approve(b)
            sender.wait(lambda s: s['phase'] == 'sent')
            receiver.wait(lambda s: s['phase'] == 'received')
            assert (receiver.output / 'remote.txt').read_bytes() == b'verified remote bytes'
            receiver.request('/api/tailcat/stop', {})
            assert not receiver.wait(lambda s: not s['tailcat']['active'])['tailcat']['invite']
            receiver.request('/api/tailcat/start', {})
            second = receiver.wait(lambda s: s['tailcat']['enabled'])['tailcat']['invite']
            assert second != invite, 'New session must get a fresh capability'
            sender.prepare('declined.txt', [('declined.txt', b'do not publish')])
            sender.request('/api/send', {'to': second})
            outgoing = sender.wait(lambda s: s['pending'])['pending']
            incoming = receiver.wait(lambda s: s['pending'])['pending']
            assert outgoing['code'] == incoming['code']
            sender.approve(outgoing)
            receiver.approve(incoming, False)
            sender.wait(lambda s: s['phase'] == 'failed')
            assert not (receiver.output / 'declined.txt').exists()
            print('PASS Tailcat consent, verified delivery, rejection, capability rotation and shutdown')
        finally:
            sender.close(); receiver.close()
if __name__ == '__main__': run()
