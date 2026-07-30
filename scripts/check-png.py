"""Decode a PNG with Python's own zlib, and refuse it if anything is wrong.

**The point is that this is not our code.** The PNG writer in this repository
is about two hundred lines including its own deflate, which is a thing to be
suspicious of — and a compressor checked by its own reader agrees with itself.
So this uses `zlib`, verifies every chunk CRC, inflates the image data, undoes
the row filters and reports what came out. A stream that only looked like
deflate, or a CRC computed with the wrong polynomial, fails here rather than in
somebody's image viewer.

Used by `smoke.sh` on the output of the real `screenshot` command.
"""
import struct, sys, zlib
d = open(sys.argv[1], 'rb').read()
if d[:8] != b'\x89PNG\r\n\x1a\n':
    sys.exit("not a PNG: bad signature")
i, chunks, order = 8, {}, []
while i < len(d):
    (ln,) = struct.unpack('>I', d[i:i+4]); kind = d[i+4:i+8]
    payload = d[i+8:i+8+ln]
    (crc,) = struct.unpack('>I', d[i+8+ln:i+12+ln])
    if crc != zlib.crc32(kind + payload) & 0xFFFFFFFF:
        sys.exit(f"bad CRC on {kind.decode()}")
    chunks[kind] = chunks.get(kind, b'') + payload
    order.append(kind.decode())
    i += 12 + ln
w, h, depth, ctype, comp, filt, inter = struct.unpack('>IIBBBBB', chunks[b'IHDR'])
if (depth, ctype, comp, filt, inter) != (8, 2, 0, 0, 0):
    sys.exit(f"unexpected IHDR: {depth} {ctype} {comp} {filt} {inter}")
raw = zlib.decompress(chunks[b'IDAT'])
if len(raw) != h * (1 + w * 3):
    sys.exit(f"inflated {len(raw)}, expected {h*(1+w*3)}")
# Undo the filters so a corrupt image cannot pass by being the right length.
out, prev = bytearray(), bytes(w * 3)
for y in range(h):
    row = raw[y*(1+w*3):(y+1)*(1+w*3)]
    if row[0] != 2:
        sys.exit(f"row {y} used filter {row[0]}, expected 2")
    cur = bytes((row[1+k] + prev[k]) & 0xFF for k in range(w*3))
    out += cur; prev = cur
distinct = len(set(bytes(out[k:k+3]) for k in range(0, len(out), 3)))
print(f"{w}x{h} {len(d)} bytes on disk, {len(out)} raw, {distinct} distinct colours, chunks {'+'.join(order)}")
