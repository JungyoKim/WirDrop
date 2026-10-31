"""Draws WirDrop's icons and package logos. Run: python make-icons.py (needs Pillow)."""
from pathlib import Path
from PIL import Image, ImageDraw

HERE = Path(__file__).resolve().parent.parent / "res"
ASSETS = HERE.parents[2] / "packaging" / "explorer-menu" / "Assets"
SS = 8  # supersampling


def lerp(a, b, t):
    return tuple(round(a[i] + (b[i] - a[i]) * t) for i in range(3))


def badge(px, top, bottom):
    s = px * SS
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    grad = Image.new("RGBA", (s, s))
    gd = ImageDraw.Draw(grad)
    for y in range(s):
        gd.line([(0, y), (s, y)], fill=lerp(top, bottom, y / (s - 1)) + (255,))
    mask = Image.new("L", (s, s), 0)
    ImageDraw.Draw(mask).ellipse([0, 0, s - 1, s - 1], fill=255)
    img.paste(grad, (0, 0), mask)
    # A dot with ripples spreading out of it: something being shared nearby.
    ring = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(ring)
    c = s * 0.5
    width = round(s * (0.085 if px <= 20 else 0.065 if px <= 32 else 0.055))
    rings = [(0.27, 255)] if px <= 20 else [(0.215, 255), (0.335, 150)]
    for r, alpha in rings:
        rr = s * r
        d.ellipse([c - rr, c - rr, c + rr, c + rr], outline=(255, 255, 255, alpha), width=width)
    dot = s * (0.11 if px <= 20 else 0.095)
    d.ellipse([c - dot, c - dot, c + dot, c + dot], fill="white")
    img.alpha_composite(ring)
    return img.resize((px, px), Image.LANCZOS)


def phone(px):
    s = px * SS
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    w, h = s * 0.56, s * 0.9
    x0, y0 = (s - w) / 2, (s - h) / 2
    d.rounded_rectangle([x0, y0, x0 + w, y0 + h], radius=s * 0.12, fill=(10, 108, 255, 255))
    m = s * 0.06
    d.rounded_rectangle([x0 + m, y0 + m, x0 + w - m, y0 + h - m], radius=s * 0.07, fill=(220, 235, 255, 255))
    nw = w * 0.34
    d.rounded_rectangle([s / 2 - nw / 2, y0 + m * 1.6, s / 2 + nw / 2, y0 + m * 2.6], radius=s * 0.03, fill=(10, 108, 255, 255))
    return img.resize((px, px), Image.LANCZOS)


def save_ico(path, draw):
    sizes = [16, 20, 24, 32, 40, 48, 64, 256]
    imgs = [draw(n) for n in sizes]
    imgs[-1].save(path, sizes=[(n, n) for n in sizes], append_images=imgs[:-1])


BLUE = ((88, 170, 255), (0, 102, 255))
GRAY = ((170, 170, 170), (110, 110, 110))
save_ico(HERE / "wirdrop.ico", lambda n: badge(n, *BLUE))
save_ico(HERE / "wirdrop-off.ico", lambda n: badge(n, *GRAY))
save_ico(HERE / "phone.ico", phone)
ASSETS.mkdir(parents=True, exist_ok=True)
for name, n in [("Square44x44Logo.png", 44), ("Square150x150Logo.png", 150), ("StoreLogo.png", 50)]:
    badge(n, *BLUE).save(ASSETS / name)
print("icons written")
