#!/usr/bin/env python3
"""Extract glyph outlines from a TrueType font into spectro's compact format.

Usage: fontgen.py FONT.ttf OUT.bin

Only the standard library is used. The output holds quadratic outlines for a
fixed character set (ASCII, Latin-1 and a few typographic marks); the runtime
rasterizer in spectro-core reads it. Kerning and hinting are dropped.

Format (little endian):
    b"SPF1" u16 units_per_em, i16 ascender, i16 descender, u16 glyph_count
    per glyph: u32 codepoint, u16 advance, u16 contour_count
        per contour: u16 point_count, then point_count * (i16 x, i16 y, u8 on_curve)
"""
import struct
import sys

CODEPOINTS = (
    list(range(0x20, 0x7F))
    + list(range(0xA0, 0x100))
    + [0x2013, 0x2014, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2026, 0x2212]
)


class Font:
    def __init__(self, data):
        self.d = data
        n = self.u16(4)
        self.tables = {}
        for i in range(n):
            rec = 12 + 16 * i
            tag = data[rec : rec + 4].decode("latin-1")
            self.tables[tag] = self.u32(rec + 8)
        head = self.tables["head"]
        self.upem = self.u16(head + 18)
        self.long_loca = self.i16(head + 50) == 1
        hhea = self.tables["hhea"]
        self.ascender = self.i16(hhea + 4)
        self.descender = self.i16(hhea + 6)
        self.n_hmetrics = self.u16(hhea + 34)
        self.cmap = self.read_cmap()

    def u8(self, o):
        return self.d[o]

    def u16(self, o):
        return struct.unpack_from(">H", self.d, o)[0]

    def i16(self, o):
        return struct.unpack_from(">h", self.d, o)[0]

    def u32(self, o):
        return struct.unpack_from(">I", self.d, o)[0]

    def read_cmap(self):
        base = self.tables["cmap"]
        best = None
        for i in range(self.u16(base + 2)):
            rec = base + 4 + 8 * i
            plat, enc, off = self.u16(rec), self.u16(rec + 2), self.u32(rec + 4)
            fmt = self.u16(base + off)
            if plat == 3 and enc == 10 and fmt == 12:
                best = (12, base + off)
            elif plat == 3 and enc == 1 and fmt == 4 and best is None:
                best = (4, base + off)
        fmt, t = best
        m = {}
        if fmt == 12:
            for g in range(self.u32(t + 12)):
                rec = t + 16 + 12 * g
                start, end, gid = self.u32(rec), self.u32(rec + 4), self.u32(rec + 8)
                for cp in range(start, end + 1):
                    m[cp] = gid + cp - start
        else:
            segs = self.u16(t + 6) // 2
            ends, starts = t + 14, t + 16 + 2 * segs
            deltas, ranges = starts + 2 * segs, starts + 4 * segs
            for s in range(segs):
                end, start = self.u16(ends + 2 * s), self.u16(starts + 2 * s)
                delta, ro = self.i16(deltas + 2 * s), self.u16(ranges + 2 * s)
                for cp in range(start, end + 1):
                    if ro == 0:
                        gid = (cp + delta) & 0xFFFF
                    else:
                        gid = self.u16(ranges + 2 * s + ro + 2 * (cp - start))
                        gid = (gid + delta) & 0xFFFF if gid else 0
                    m[cp] = gid
        return m

    def advance(self, gid):
        i = min(gid, self.n_hmetrics - 1)
        return self.u16(self.tables["hmtx"] + 4 * i)

    def glyph_range(self, gid):
        loca = self.tables["loca"]
        if self.long_loca:
            a, b = self.u32(loca + 4 * gid), self.u32(loca + 4 * gid + 4)
        else:
            a, b = 2 * self.u16(loca + 2 * gid), 2 * self.u16(loca + 2 * gid + 2)
        return self.tables["glyf"] + a, b - a

    def contours(self, gid, depth=0):
        """Return a list of contours, each a list of (x, y, on_curve)."""
        o, length = self.glyph_range(gid)
        if length == 0:
            return []
        n = self.i16(o)
        if n >= 0:
            return self.simple(o, n)
        assert depth < 8, "composite glyph nesting too deep"
        return self.composite(o, depth)

    def simple(self, o, n):
        ends = [self.u16(o + 10 + 2 * i) for i in range(n)]
        npts = ends[-1] + 1 if ends else 0
        p = o + 10 + 2 * n
        p += 2 + self.u16(p)
        flags = []
        while len(flags) < npts:
            f = self.u8(p)
            p += 1
            flags.append(f)
            if f & 8:
                r = self.u8(p)
                p += 1
                flags.extend([f] * r)
        xs, ys = [], []
        for coords, short_bit, same_bit in ((xs, 2, 16), (ys, 4, 32)):
            v = 0
            for f in flags:
                if f & short_bit:
                    d = self.u8(p)
                    p += 1
                    v += d if f & same_bit else -d
                elif not f & same_bit:
                    v += self.i16(p)
                    p += 2
                coords.append(v)
        out, start = [], 0
        for e in ends:
            out.append([(xs[i], ys[i], flags[i] & 1) for i in range(start, e + 1)])
            start = e + 1
        return out

    def composite(self, o, depth):
        p = o + 10
        out = []
        while True:
            flags, gid = self.u16(p), self.u16(p + 2)
            p += 4
            if flags & 1:
                a, b = self.i16(p), self.i16(p + 2)
                p += 4
            else:
                a, b = struct.unpack_from(">bb", self.d, p)
                p += 2
            assert flags & 2, "point-matched components are unsupported"
            xx, xy, yx, yy = 1.0, 0.0, 0.0, 1.0
            f2 = lambda q: self.i16(q) / 16384.0
            if flags & 8:
                xx = yy = f2(p)
                p += 2
            elif flags & 0x40:
                xx, yy = f2(p), f2(p + 2)
                p += 4
            elif flags & 0x80:
                xx, xy, yx, yy = f2(p), f2(p + 2), f2(p + 4), f2(p + 6)
                p += 8
            for c in self.contours(gid, depth + 1):
                out.append(
                    [
                        (round(x * xx + y * yx + a), round(x * xy + y * yy + b), on)
                        for x, y, on in c
                    ]
                )
            if not flags & 0x20:
                return out


def main():
    src, dst = sys.argv[1], sys.argv[2]
    with open(src, "rb") as f:
        font = Font(f.read())
    glyphs = [cp for cp in CODEPOINTS if font.cmap.get(cp)]
    out = bytearray(b"SPF1")
    out += struct.pack("<HhhH", font.upem, font.ascender, font.descender, len(glyphs))
    for cp in glyphs:
        gid = font.cmap[cp]
        cs = font.contours(gid)
        out += struct.pack("<IHH", cp, font.advance(gid), len(cs))
        for c in cs:
            out += struct.pack("<H", len(c))
            for x, y, on in c:
                out += struct.pack("<hhB", x, y, on)
    with open(dst, "wb") as f:
        f.write(out)
    print(f"{dst}: {len(glyphs)} glyphs, {len(out)} bytes")


if __name__ == "__main__":
    main()
