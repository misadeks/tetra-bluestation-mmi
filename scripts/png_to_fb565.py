#!/usr/bin/env python3
"""Convert a PNG to a raw framebuffer image for a direct /dev/fb0 write.

Used by the Pi boot splash: writing raw pixels straight to /dev/fb0 shows on
the panel regardless of the foreground VT and persists in the framebuffer until
the kiosk's DRM modeset takes over - unlike `fbi`, which renders on a VT and
clears the screen when it exits. See deploy/splash.service and PI_SETUP.md.

Stdlib only (Raspberry Pi OS Lite has python3 but no Pillow/ImageMagick).
Supports 8-bit non-interlaced PNG, colour type 2 (RGB) or 6 (RGBA). Reads the
target geometry (width/height/stride) from /sys/class/graphics/fb0 and packs
RGB565 little-endian (bits_per_pixel must be 16, the Pi KMS default). Images
that don't match the panel size are centred on black (cropped if larger).

Usage: png_to_fb565.py INPUT.png OUTPUT.raw [--fb /dev/fb0-sysfs-name]
"""
import struct
import sys
import zlib


def read_png(path):
    with open(path, "rb") as f:
        data = f.read()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError("not a PNG")
    pos = 8
    width = height = bit_depth = color_type = interlace = None
    idat = bytearray()
    while pos < len(data):
        (length,) = struct.unpack(">I", data[pos:pos + 4])
        ctype = data[pos + 4:pos + 8]
        body = data[pos + 8:pos + 8 + length]
        pos += 12 + length  # 4 len + 4 type + body + 4 crc
        if ctype == b"IHDR":
            width, height, bit_depth, color_type, _comp, _filt, interlace = \
                struct.unpack(">IIBBBBB", body)
        elif ctype == b"IDAT":
            idat += body
        elif ctype == b"IEND":
            break
    if bit_depth not in (8, 16) or color_type not in (2, 6) or interlace != 0:
        raise ValueError(
            "unsupported PNG (need 8/16-bit non-interlaced RGB/RGBA; got depth="
            f"{bit_depth} color_type={color_type} interlace={interlace})")
    channels = 3 if color_type == 2 else 4
    bpc = bit_depth // 8          # bytes per channel sample (1 or 2)
    pixbytes = channels * bpc     # bytes per pixel (filter left-offset)
    raw = zlib.decompress(bytes(idat))
    stride = width * pixbytes
    # Undo PNG scanline filters (per row: filter byte + stride bytes).
    out = bytearray(height * stride)
    prev = bytearray(stride)
    p = 0
    for y in range(height):
        ftype = raw[p]; p += 1
        line = bytearray(raw[p:p + stride]); p += stride
        if ftype == 1:  # Sub
            for i in range(pixbytes, stride):
                line[i] = (line[i] + line[i - pixbytes]) & 0xFF
        elif ftype == 2:  # Up
            for i in range(stride):
                line[i] = (line[i] + prev[i]) & 0xFF
        elif ftype == 3:  # Average
            for i in range(stride):
                a = line[i - pixbytes] if i >= pixbytes else 0
                line[i] = (line[i] + ((a + prev[i]) >> 1)) & 0xFF
        elif ftype == 4:  # Paeth
            for i in range(stride):
                a = line[i - pixbytes] if i >= pixbytes else 0
                b = prev[i]
                c = prev[i - pixbytes] if i >= pixbytes else 0
                pa = abs(b - c); pb = abs(a - c); pc = abs(a + b - 2 * c)
                pr = a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
                line[i] = (line[i] + pr) & 0xFF
        elif ftype != 0:
            raise ValueError(f"bad filter type {ftype}")
        out[y * stride:(y + 1) * stride] = line
        prev = line
    return width, height, channels, bpc, out


def read_int(path, default=None):
    try:
        with open(path) as f:
            return f.read().strip()
    except OSError:
        return default


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    inp, outp = sys.argv[1], sys.argv[2]
    fbdir = "/sys/class/graphics/fb0"
    size = read_int(f"{fbdir}/virtual_size", "720,720")
    fb_w, fb_h = (int(x) for x in size.split(","))
    bpp = int(read_int(f"{fbdir}/bits_per_pixel", "16"))
    stride = int(read_int(f"{fbdir}/stride", str(fb_w * 2)))
    if bpp != 16:
        sys.exit(f"framebuffer is {bpp}bpp; this converter only does 16bpp RGB565")

    iw, ih, ch, bpc, px = read_png(inp)
    pixstep = ch * bpc          # bytes per pixel in the decoded buffer
    rowbytes = iw * pixstep
    fb = bytearray(stride * fb_h)  # zero-filled = black
    # Centre the image on the panel (crop if larger).
    ox = (fb_w - iw) // 2
    oy = (fb_h - ih) // 2
    for y in range(ih):
        ty = y + oy
        if ty < 0 or ty >= fb_h:
            continue
        row = y * rowbytes
        base = ty * stride
        for x in range(iw):
            tx = x + ox
            if tx < 0 or tx >= fb_w:
                continue
            s = row + x * pixstep
            # High byte of each channel (8-bit value; MSB for 16-bit BE samples).
            r, g, b = px[s], px[s + bpc], px[s + 2 * bpc]
            v = ((r & 0xF8) << 8) | ((g & 0xFC) << 3) | (b >> 3)
            o = base + tx * 2
            fb[o] = v & 0xFF
            fb[o + 1] = (v >> 8) & 0xFF
    with open(outp, "wb") as f:
        f.write(fb)
    print(f"wrote {outp}: {fb_w}x{fb_h} RGB565 stride={stride} ({len(fb)} bytes)")


if __name__ == "__main__":
    main()
