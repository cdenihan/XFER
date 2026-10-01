#!/usr/bin/env python3
"""Generate the original XFER icon assets using only Python's standard library."""
from pathlib import Path
import math
import struct
import zlib

ROOT = Path(__file__).resolve().parent.parent / 'desktop/assets'
TOP = [(235, 290), (625, 290), (625, 195), (805, 375), (625, 555), (625, 460), (235, 460)]
BOTTOM = [(765, 710), (375, 710), (375, 805), (195, 625), (375, 445), (375, 540), (765, 540)]


def inside(x, y, polygon):
    result = False
    previous = polygon[-1]
    for current in polygon:
        ax, ay = previous
        bx, by = current
        if (ay > y) != (by > y) and x < (bx - ax) * (y - ay) / (by - ay) + ax:
            result = not result
        previous = current
    return result


def pixel(x, y):
    # Rounded tile with a restrained diagonal gradient; transparent outside.
    dx = max(abs(x - 500) - 260, 0)
    dy = max(abs(y - 500) - 260, 0)
    if math.hypot(dx, dy) > 200:
        return (0, 0, 0, 0)
    if inside(x, y, TOP):
        return (99, 230, 201, 255)
    if inside(x, y, BOTTOM):
        return (142, 184, 255, 255)
    mix = (x + y) / 2000
    return (int(30 - 15 * mix), int(46 - 24 * mix), int(63 - 31 * mix), 255)


def png(size):
    rows = bytearray()
    for row in range(size):
        rows.append(0)
        for column in range(size):
            samples = [pixel((column + x) * 1000 / size, (row + y) * 1000 / size)
                       for x, y in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)]]
            alpha = sum(value[3] for value in samples)
            color = [round(sum(value[channel] * value[3] for value in samples) / alpha)
                     if alpha else 0 for channel in range(3)]
            rows.extend((*color, round(alpha / 4)))
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))
    return b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', size, size, 8, 6, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(rows, 9)) + chunk(b'IEND', b'')


def main():
    ROOT.mkdir(parents=True, exist_ok=True)
    paths = [' '.join(f'{x},{y}' for x, y in shape) for shape in [TOP, BOTTOM]]
    (ROOT / 'xfer.svg').write_text(f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1000 1000">
  <title>XFER — direct exchange</title>
  <defs><linearGradient id="tile" x2="1" y2="1"><stop stop-color="#1e2e3f"/><stop offset="1" stop-color="#0f1620"/></linearGradient></defs>
  <rect x="40" y="40" width="920" height="920" rx="200" fill="url(#tile)"/>
  <polygon points="{paths[0]}" fill="#63e6c9"/>
  <polygon points="{paths[1]}" fill="#8eb8ff"/>
</svg>
''')
    sizes = [16, 32, 48, 64, 128, 256, 512, 1024]
    images = {size: png(size) for size in sizes}
    (ROOT / 'xfer.png').write_bytes(images[256])
    # ICNS accepts PNG payloads at these standard sizes.
    kinds = {16: b'icp4', 32: b'icp5', 64: b'icp6', 128: b'ic07', 256: b'ic08', 512: b'ic09', 1024: b'ic10'}
    body = b''.join(kind + struct.pack('>I', len(images[size]) + 8) + images[size] for size, kind in kinds.items())
    (ROOT / 'xfer.icns').write_bytes(b'icns' + struct.pack('>I', len(body) + 8) + body)
    windows = [16, 32, 48, 64, 128, 256]
    offset = 6 + 16 * len(windows)
    entries = bytearray()
    for size in windows:
        entries.extend(struct.pack('<BBBBHHII', size % 256, size % 256, 0, 0, 1, 32, len(images[size]), offset))
        offset += len(images[size])
    (ROOT / 'xfer.ico').write_bytes(struct.pack('<HHH', 0, 1, len(windows)) + entries + b''.join(images[size] for size in windows))


if __name__ == '__main__':
    main()
