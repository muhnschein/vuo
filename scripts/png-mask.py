#!/usr/bin/env python3
"""Reduce an RGBA PNG to a grayscale coverage mask.

The text art is painted in one colour, so every pixel's RGB is the same and
only the coverage differs: three of the four channels are redundant. This
keeps the alpha channel as the single channel of a grayscale PNG, which is
what qml/components/TextArt.qml samples and tints.

The coverage is quantised on the way, because it is antialiasing rather than
photography: sixteen levels are indistinguishable once the app has multiplied
them by an ink of about a half, and they cost a little over half of what the
full range does -- measured at 960 KiB against 558 KiB for the page.

Pure standard library on purpose. This runs from `make textart`, which a
contributor may run on any machine, and a Pillow that has to be installed
first is a reason not to regenerate the art.
"""
import struct, sys, zlib


def chunks(data):
    i = 8
    while i < len(data):
        (length,) = struct.unpack(">I", data[i:i + 4])
        kind = data[i + 4:i + 8]
        yield kind, data[i + 8:i + 8 + length]
        i += 8 + length + 4


def read_rgba(path):
    data = open(path, "rb").read()
    assert data[:8] == b"\x89PNG\r\n\x1a\n", "not a PNG"
    idat = b""
    width = height = depth = ctype = None
    for kind, body in chunks(data):
        if kind == b"IHDR":
            width, height, depth, ctype, _, _, interlace = struct.unpack(">IIBBBBB", body)
            assert (depth, ctype, interlace) == (8, 6, 0), (depth, ctype, interlace)
        elif kind == b"IDAT":
            idat += body
    raw = zlib.decompress(idat)
    stride = width * 4
    out = bytearray(width * height * 4)
    prev = bytearray(stride)
    pos = 0
    for y in range(height):
        f = raw[pos]; pos += 1
        line = bytearray(raw[pos:pos + stride]); pos += stride
        if f == 1:
            for x in range(4, stride):
                line[x] = (line[x] + line[x - 4]) & 0xFF
        elif f == 2:
            for x in range(stride):
                line[x] = (line[x] + prev[x]) & 0xFF
        elif f == 3:
            for x in range(stride):
                left = line[x - 4] if x >= 4 else 0
                line[x] = (line[x] + ((left + prev[x]) >> 1)) & 0xFF
        elif f == 4:
            for x in range(stride):
                a = line[x - 4] if x >= 4 else 0
                b = prev[x]
                c = prev[x - 4] if x >= 4 else 0
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                pr = a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
                line[x] = (line[x] + pr) & 0xFF
        out[y * stride:(y + 1) * stride] = line
        prev = line
    return width, height, bytes(out)


def write_gray(path, width, height, gray):
    def chunk(kind, body):
        return (struct.pack(">I", len(body)) + kind + body
                + struct.pack(">I", zlib.crc32(kind + body) & 0xFFFFFFFF))

    raw = bytearray()
    prev = bytearray(width)
    for y in range(height):
        line = gray[y * width:(y + 1) * width]
        # The usual heuristic: try each filter, keep the one whose bytes sum
        # smallest, which is what deflate compresses best.
        best, best_score, best_f = None, None, 0
        for f in range(5):
            cand = bytearray(width)
            for x in range(width):
                a = line[x - 1] if x >= 1 else 0
                b = prev[x]
                c = prev[x - 1] if x >= 1 else 0
                v = line[x]
                if f == 0:
                    cand[x] = v
                elif f == 1:
                    cand[x] = (v - a) & 0xFF
                elif f == 2:
                    cand[x] = (v - b) & 0xFF
                elif f == 3:
                    cand[x] = (v - ((a + b) >> 1)) & 0xFF
                else:
                    p = a + b - c
                    pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                    pr = a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
                    cand[x] = (v - pr) & 0xFF
            score = sum(v if v < 128 else 256 - v for v in cand)
            if best_score is None or score < best_score:
                best, best_score, best_f = cand, score, f
        raw.append(best_f)
        raw += best
        prev = bytearray(line)

    body = (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 0, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
            + chunk(b"IEND", b""))
    open(path, "wb").write(body)


LEVELS = 16


def quantise(values, levels):
    step = 255 / (levels - 1)
    table = bytes(min(255, int(round(round(v / step) * step))) for v in range(256))
    return bytes(table[v] for v in values)


if __name__ == "__main__":
    src, dst = sys.argv[1], sys.argv[2]
    w, h, rgba = read_rgba(src)
    coverage = quantise(bytes(rgba[i * 4 + 3] for i in range(w * h)), LEVELS)
    write_gray(dst, w, h, coverage)
    print("%s  %dx%d  %d KiB" % (dst, w, h, len(open(dst, "rb").read()) // 1024))
