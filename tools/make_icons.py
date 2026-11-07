import os
from PIL import Image, ImageDraw

# Writes straight into the Tauri icon folder so `npx tauri icon` is not needed.
OUT = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "src-tauri", "icons")
SIZE = 1024
S = 4  # supersample

def rounded(draw, box, r, fill):
    draw.rounded_rectangle(box, radius=r, fill=fill)

def make():
    n = SIZE * S
    img = Image.new("RGBA", (n, n), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    m = int(n * 0.06)
    r = int(n * 0.22)
    rounded(d, (m, m, n - m, n - m), r, (28, 28, 32, 255))

    # clipboard body
    pad = int(n * 0.22)
    top = int(n * 0.20)
    body = (pad, top, n - pad, n - int(n * 0.16))
    rounded(d, body, int(n * 0.09), (242, 242, 247, 255))

    # clip at top
    cw = int(n * 0.22)
    ch = int(n * 0.10)
    cx = n // 2
    rounded(d, (cx - cw // 2, top - ch // 2, cx + cw // 2, top + ch // 2 + int(n * 0.02)),
            int(n * 0.035), (10, 132, 255, 255))

    # text lines
    lx = body[0] + int(n * 0.10)
    lw = (body[2] - body[0]) - int(n * 0.20)
    ys = [n * 0.42, n * 0.54, n * 0.66]
    for i, y in enumerate(ys):
        w = lw if i < 2 else int(lw * 0.62)
        d.rounded_rectangle((lx, int(y), lx + w, int(y) + int(n * 0.045)),
                            radius=int(n * 0.023), fill=(60, 60, 67, 255))

    return img.resize((SIZE, SIZE), Image.LANCZOS)

n = SIZE * S
base = make()
base.save(os.path.join(OUT, "icon.png"))
for s in (32, 128, 256, 512):
    base.resize((s, s), Image.LANCZOS).save(os.path.join(OUT, f"{s}x{s}.png"))
# 32x32@2x style name used by tauri templates
base.resize((64, 64), Image.LANCZOS).save(os.path.join(OUT, "32x32@2x.png"))
base.resize((256, 256), Image.LANCZOS).save(
    os.path.join(OUT, "icon.ico"),
    format="ICO", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
)
# tray icon: monochrome-ish light glyph on transparent
tray = Image.new("RGBA", (n, n), (0, 0, 0, 0))
td = ImageDraw.Draw(tray)
pad = int(n * 0.14)
top = int(n * 0.16)
body = (pad, top, n - pad, n - int(n * 0.10))
rounded(td, body, int(n * 0.14), (255, 255, 255, 255))
cw = int(n * 0.26)
ch = int(n * 0.12)
cx = n // 2
rounded(td, (cx - cw // 2, top - ch // 2, cx + cw // 2, top + ch // 2 + int(n * 0.02)),
        int(n * 0.05), (255, 255, 255, 255))
lx = body[0] + int(n * 0.13)
lw = (body[2] - body[0]) - int(n * 0.26)
for i, y in enumerate([n * 0.42, n * 0.56]):
    w = lw if i == 0 else int(lw * 0.6)
    td.rounded_rectangle((lx, int(y), lx + w, int(y) + int(n * 0.07)),
                         radius=int(n * 0.035), fill=(20, 20, 24, 255))
tray.resize((64, 64), Image.LANCZOS).save(os.path.join(OUT, "tray.png"))
print("ok", os.listdir(OUT))
