"""Writes testbins/bin/VAULT.DAT, the data file of VAULT.EXE (src/vault.c): an archive of named members,
each stored, run-length coded (PCX's code) or LZSS coded (Okumura's lzss.c: a 4096-byte ring of spaces,
matches of 3 to 18). The output is the same byte for byte at every run; it is committed, so tests don't
need Python.

    python testbins/data.py
"""
import os
import struct

CELLS = 32
RING, LONGEST = 4096, 18


def runs(data):
    """PCX's run-length code: a byte with its top two bits set repeats the next byte its low six times."""
    out = bytearray()
    i = 0
    while i < len(data):
        n = 1
        while i + n < len(data) and data[i + n] == data[i] and n < 63:
            n += 1
        if n > 1 or data[i] >= 0xC0:
            out += bytes([0xC0 | n, data[i]])
        else:
            out.append(data[i])
        i += n
    return bytes(out)


def lzss(data):
    """Okumura's LZSS, greedy: the longest match in the ring that doesn't overlap what it writes."""
    ring = bytearray(b" " * RING)
    r = RING - LONGEST
    valid = set(range(RING - LONGEST))
    # The ring's places by the byte they hold, to try only those a match can start at.
    places = {0x20: list(range(RING - LONGEST))}
    out = bytearray()
    flags, items, bit = 0, bytearray(), 0
    i = 0
    while i < len(data):
        best, place = 0, 0
        for p in places.get(data[i], []):
            if ring[p] != data[i]:
                continue
            n = 0
            while (
                n < LONGEST
                and i + n < len(data)
                and ((r - p) & (RING - 1)) > n
                and ((p + n) & (RING - 1)) in valid
                and ring[(p + n) & (RING - 1)] == data[i + n]
            ):
                n += 1
            if n > best or (n == best and p < place):
                best, place = n, p
        if best >= 3:
            items += bytes([place & 0xFF, ((place >> 4) & 0xF0) | (best - 3)])
            count = best
        else:
            flags |= 1 << bit
            items.append(data[i])
            count = 1
        for k in range(count):
            ring[r] = data[i + k]
            valid.add(r)
            places.setdefault(data[i + k], []).append(r)
            r = (r + 1) & (RING - 1)
        i += count
        bit += 1
        if bit == 8:
            out.append(flags)
            out += items
            flags, items, bit = 0, bytearray(), 0
    if bit:
        out.append(flags)
        out += items
    return bytes(out)


def title():
    """320 x 200: bands by row, a frame, and blocks spelling nothing in particular."""
    pixels = bytearray(320 * 200)
    for y in range(200):
        for x in range(320):
            c = 32 + y // 8
            if 20 <= x < 300 and 20 <= y < 180 and (x in (20, 299) or y in (20, 179)):
                c = 15
            if 60 <= y < 100 and 40 <= x < 280 and (x // 20) % 2 == 0:
                c = 100 + (x // 20)
            pixels[y * 320 + x] = c
    return bytes(pixels)


def palette():
    """256 colours of 6 bits: a red ramp, a green one by fours, a blue one down."""
    out = bytearray()
    for i in range(256):
        out += bytes([i * 63 // 255, (i * 4) % 64, (255 - i) * 63 // 255])
    return bytes(out)


def level(start, exit, walls):
    cells = bytearray(CELLS * CELLS)
    for y in range(CELLS):
        for x in range(CELLS):
            if x in (0, CELLS - 1) or y in (0, CELLS - 1):
                cells[y * CELLS + x] = 1
    for x, y in walls:
        cells[y * CELLS + x] = 1
    cells[start[1] * CELLS + start[0]] = 3
    cells[exit[1] * CELLS + exit[0]] = 2
    return bytes(cells)


def main():
    members = [
        ("TITLE", 1, title()),
        ("PALETTE", 0, palette()),
        # Level 1: a wall between the start and the exit, the way round below it.
        ("LEVEL1", 2, level((2, 2), (6, 2), [(4, 2), (4, 1), (8, 3), (9, 3), (10, 3)] + [(x, 12) for x in range(3, 29)])),
        # Level 2: the exit three cells below the start.
        ("LEVEL2", 2, level((10, 10), (10, 13), [(11, 11), (9, 11)] + [(20, y) for y in range(1, 20)])),
    ]
    header = 6 + 24 * len(members)
    directory, body = bytearray(), bytearray()
    for name, method, data in members:
        packed = {0: lambda d: d, 1: runs, 2: lzss}[method](data)
        directory += struct.pack("<8sIIIHH", name.encode(), header + len(body), len(packed), len(data), method, 0)
        body += packed
    out = b"VALT" + struct.pack("<H", len(members)) + bytes(directory) + bytes(body)
    path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "bin", "VAULT.DAT")
    with open(path, "wb") as f:
        f.write(out)
    print(f"{path}: {len(out)} bytes, members {', '.join(n for n, _, _ in members)}")


if __name__ == "__main__":
    main()
