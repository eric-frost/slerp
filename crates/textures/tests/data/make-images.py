#!/usr/bin/env python3
"""Writes the small test images in this folder with Pillow, plus each one's
pixels as Pillow decodes them (`<name>.rgba`, 8-bit RGBA, rows top to
bottom), so the tests compare our readers against an independent decoder.
Run it from this folder; the outputs are committed."""

from PIL import Image

W, H = 23, 17


def gradient():
    im = Image.new("RGBA", (W, H))
    for y in range(H):
        for x in range(W):
            im.putpixel((x, y), (x * 11, y * 15, (x * y * 3) % 256, 255 - x * 5))
    return im


def save(im, name, **kw):
    im.save(name, **kw)
    ref = Image.open(name).convert("RGBA")
    open(name + ".rgba", "wb").write(ref.tobytes())


g = gradient()
rgb = g.convert("RGB")
pal = rgb.quantize(16)
save(rgb, "rgb.jpg", quality=95, subsampling=0)
save(rgb, "rgb420.jpg", quality=90, subsampling=2)
save(g.convert("L"), "grey.jpg", quality=95)
save(g, "rgba-lossless.webp", lossless=True)
save(rgb, "rgb-lossy.webp", quality=90)
second = Image.new("RGBA", (W, H), (255, 0, 0, 255))
g.save("anim.webp", save_all=True, append_images=[second], lossless=True, duration=100)
open("anim.webp.rgba", "wb").write(Image.open("anim.webp").convert("RGBA").tobytes())
save(rgb, "rgb24.bmp")
save(g, "rgba32.bmp")
save(pal, "pal8.bmp")
save(pal.convert("1"), "mono1.bmp")
save(rgb, "rgb.tga")
save(g, "rgba-rle.tga", compression="tga_rle")
save(g.convert("L"), "grey.tga")
save(pal, "pal.tga")
save(rgb, "rgb.pcx")
save(pal, "pal8.pcx")
save(g.convert("1"), "mono.pcx")
save(g, "rgba.png")
save(pal, "pal.png", transparency=3)
save(g.convert("LA"), "grey-alpha.png")
save(rgb, "interlaced.png", interlace=1)
im16 = Image.new("I;16", (W, H))
for y in range(H):
    for x in range(W):
        im16.putpixel((x, y), x * 2800 + y * 100)
im16.save("grey16.png")
open("grey16.png.rgba", "wb").write(
    bytes(v for y in range(H) for x in range(W) for v in [((x * 2800 + y * 100) >> 8)] * 3 + [255])
)
