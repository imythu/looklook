"""渲染客户端图标（assets/icon-client/icon.svg 的 PIL 版本；ImageMagick 的 SVG 渲染器不支持渐变）。"""
from PIL import Image, ImageDraw
import math
S = 4  # 超采样
def lerp(a, b, t): return tuple(round(a[i] + (b[i] - a[i]) * t) for i in range(3))
def hexc(h): return tuple(int(h[i:i+2], 16) for i in (1, 3, 5))
def render(size, square=False):
    N = 512 * S
    img = Image.new('RGBA', (N, N))
    # 对角渐变 #10b981 → #047857(.6) → #064e3b
    stops = [(0, hexc('#10b981')), (.6, hexc('#047857')), (1, hexc('#064e3b'))]
    grad = Image.new('RGBA', (N, N))
    px = grad.load()
    for y in range(N):
        for x in range(N):
            t = (x + y) / (2 * (N - 1))
            c = stops[0][1]
            for (t0, c0), (t1, c1) in zip(stops, stops[1:]):
                if t0 <= t <= t1: c = lerp(c0, c1, (t - t0) / (t1 - t0))
            px[x, y] = c + (255,)
    # 高光
    glow = Image.new('RGBA', (N, N))
    gp = glow.load()
    cx, cy, r = .28 * N, .12 * N, .75 * N
    for y in range(N):
        for x in range(N):
            d = min(1, math.hypot(x - cx, y - cy) / r)
            gp[x, y] = (255, 255, 255, round(255 * .26 * (1 - d)))
    grad = Image.alpha_composite(grad, glow)
    mask = Image.new('L', (N, N), 255 if square else 0)
    if not square: ImageDraw.Draw(mask).rounded_rectangle([0, 0, N - 1, N - 1], radius=116 * S, fill=255)
    img.paste(grad, (0, 0), mask)
    d = ImageDraw.Draw(img)
    k = lambda v: v * S
    for cx_ in (160, 352): d.ellipse([k(cx_ - 92), k(268 - 92), k(cx_ + 92), k(268 + 92)], fill='#ffffff')
    ink = '#022c22'
    pts = [(152, 232), (192, 268), (152, 304)]
    d.line([(k(x), k(y)) for x, y in pts], fill=ink, width=k(30), joint='curve')
    for x, y in (pts[0], pts[2]): d.ellipse([k(x) - k(15), k(y) - k(15), k(x) + k(15), k(y) + k(15)], fill=ink)
    d.ellipse([k(192) - k(15), k(268) - k(15), k(192) + k(15), k(268) + k(15)], fill=ink)
    d.rounded_rectangle([k(330), k(290), k(398), k(318)], radius=k(14), fill=ink)
    d.ellipse([k(416 - 22), k(118 - 22), k(416 + 22), k(118 + 22)], fill='#fbbf24')
    return img.resize((size, size), Image.LANCZOS)
if __name__ == '__main__':
    render(512).save('icon-512.png'); render(192).save('icon-192.png')
    render(180, square=True).convert('RGB').save('apple-touch-icon.png')
    render(64).save('favicon.ico', sizes=[(16, 16), (32, 32), (48, 48)])
