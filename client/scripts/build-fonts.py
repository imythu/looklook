#!/usr/bin/env python3
"""生成终端网页字体：JetBrains Mono（英文）+ 霞鹜文楷等宽 LXGW WenKai Mono（中文）。

终端经常在手机上、经远程通道打开，所以中文字体按字频切成许多小的 WOFF2 分片，
配合 CSS `unicode-range`，浏览器只下载屏幕上真正出现的字所在的分片。
分片方式来自 scripts/data/cjk-slices.txt（常用字集中在少数分片里），剩余的字按码位补充分片。
JetBrains Mono 已经覆盖的字符（英文、符号、制表符）不再放进中文字体。

用法：
  scripts/build-fonts.py --jetbrains JetBrainsMono-2.304.zip \\
      --wenkai-regular LXGWWenKaiMono-Regular.ttf --wenkai-medium LXGWWenKaiMono-Medium.ttf \\
      --out vendor/fonts
  scripts/build-fonts.py --refresh-slices      # 重新生成 cjk-slices.txt（需要访问 Google Fonts）

依赖：fontTools、brotli（Debian: apt install python3-fonttools python3-brotli）。
"""

import argparse
import io
import os
import re
import shutil
import sys
import urllib.request
import zipfile
from concurrent.futures import ProcessPoolExecutor

HERE = os.path.dirname(os.path.abspath(__file__))
SLICES = os.path.join(HERE, "data", "cjk-slices.txt")
EXTRA_CHUNK = 300

JB_FACES = [
    ("JetBrainsMono-Regular.woff2", "normal", 400),
    ("JetBrainsMono-Bold.woff2", "normal", 700),
    ("JetBrainsMono-Italic.woff2", "italic", 400),
    ("JetBrainsMono-BoldItalic.woff2", "italic", 700),
]


def parse_ranges(text):
    cps = set()
    for part in text.split(","):
        part = part.strip()
        if not part:
            continue
        if "-" in part:
            a, b = part.split("-")
            cps.update(range(int(a, 16), int(b, 16) + 1))
        else:
            cps.add(int(part, 16))
    return cps


def load_slices():
    slices = []
    with open(SLICES, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            _, ranges = line.split(" ", 1)
            slices.append(parse_ranges(ranges))
    return slices


def to_ranges(cps):
    """码位集合 → `U+4e00-4e0f,U+4e11` 形式。"""
    out = []
    cps = sorted(cps)
    i = 0
    while i < len(cps):
        j = i
        while j + 1 < len(cps) and cps[j + 1] == cps[j] + 1:
            j += 1
        out.append(f"U+{cps[i]:x}" if i == j else f"U+{cps[i]:x}-{cps[j]:x}")
        i = j + 1
    return ",".join(out)


def cmap_of(path_or_bytes):
    from fontTools.ttLib import TTFont

    f = TTFont(io.BytesIO(path_or_bytes) if isinstance(path_or_bytes, bytes) else path_or_bytes, lazy=True)
    return set(f.getBestCmap().keys())


def subset_one(args):
    src, cps, dst = args
    from fontTools import subset
    from fontTools.ttLib import TTFont

    opts = subset.Options()
    opts.flavor = "woff2"
    opts.hinting = False
    opts.layout_features = ["ccmp", "locl", "mark", "mkmk"]
    opts.name_IDs = [0, 1, 2, 3, 4, 5, 6, 13, 14]
    opts.notdef_outline = True
    opts.drop_tables += ["meta"]
    font = TTFont(src, lazy=True)
    s = subset.Subsetter(opts)
    s.populate(unicodes=cps)
    s.subset(font)
    font.flavor = "woff2"
    font.save(dst)
    return dst, os.path.getsize(dst)


def plan_chunks(font_cps, exclude):
    """按字频分片，剩余的按码位补充分片。"""
    wanted = font_cps - exclude
    chunks, seen = [], set()
    for sl in load_slices():
        c = (sl & wanted) - seen
        if c:
            chunks.append(c)
            seen |= c
    rest = sorted(wanted - seen)
    for i in range(0, len(rest), EXTRA_CHUNK):
        chunks.append(set(rest[i : i + EXTRA_CHUNK]))
    return chunks


def license_for_wenkai(jb_ofl_text, ttf):
    from fontTools.ttLib import TTFont

    name = TTFont(ttf, lazy=True)["name"]
    copyright_line = name.getDebugName(0) or "Copyright LXGW"
    body = jb_ofl_text[jb_ofl_text.index("This Font Software is licensed") :]
    return f"{copyright_line}\n\n{body}"


def build(args):
    out = os.path.abspath(args.out)
    tmp = out + ".tmp"
    shutil.rmtree(tmp, ignore_errors=True)
    os.makedirs(os.path.join(tmp, "wenkai"))

    css = [
        "/* 看看终端字体：JetBrains Mono（英文）+ 霞鹜文楷等宽 LXGW WenKai Mono（中文，按字频分片按需加载）。",
        "   两者均使用 SIL Open Font License 1.1，许可证见同目录 LICENSE-*.txt。由 scripts/build-fonts.py 生成。 */",
    ]
    with zipfile.ZipFile(args.jetbrains) as z:
        for fname, style, weight in JB_FACES:
            data = z.read(f"fonts/webfonts/{fname}")
            with open(os.path.join(tmp, fname), "wb") as f:
                f.write(data)
            css.append(
                f'@font-face{{font-family:"JetBrains Mono";font-style:{style};font-weight:{weight};font-display:block;'
                f'src:url({fname}) format("woff2")}}'
            )
        jb_ofl = z.read("OFL.txt").decode("utf-8")
        jb_cps = cmap_of(z.read("fonts/webfonts/JetBrainsMono-Regular.woff2"))
    with open(os.path.join(tmp, "LICENSE-JetBrainsMono.txt"), "w", encoding="utf-8") as f:
        f.write(jb_ofl)
    with open(os.path.join(tmp, "LICENSE-LXGWWenKaiMono.txt"), "w", encoding="utf-8") as f:
        f.write(license_for_wenkai(jb_ofl, args.wenkai_regular))

    jobs, faces = [], []
    for weight, src in ((400, args.wenkai_regular), (700, args.wenkai_medium)):
        chunks = plan_chunks(cmap_of(src), jb_cps)
        for i, cps in enumerate(chunks):
            name = f"wenkai/{weight}-{i:03d}.woff2"
            jobs.append((src, cps, os.path.join(tmp, name)))
            faces.append((weight, name, cps))
        print(f"LXGW WenKai Mono {weight}: {len(chunks)} 个分片", file=sys.stderr)

    total = 0
    with ProcessPoolExecutor(max_workers=args.jobs) as ex:
        for n, (_, size) in enumerate(ex.map(subset_one, jobs), 1):
            total += size
            if n % 20 == 0 or n == len(jobs):
                print(f"  {n}/{len(jobs)}", file=sys.stderr)
    for weight, name, cps in faces:
        css.append(
            f'@font-face{{font-family:"LXGW WenKai Mono";font-style:normal;font-weight:{weight};font-display:swap;'
            f'src:url({name}) format("woff2");unicode-range:{to_ranges(cps)}}}'
        )
    with open(os.path.join(tmp, "fonts.css"), "w", encoding="utf-8") as f:
        f.write("\n".join(css) + "\n")

    shutil.rmtree(out, ignore_errors=True)
    os.rename(tmp, out)
    print(f"完成：{out}（中文分片共 {total / 1048576:.1f} MB）", file=sys.stderr)


def refresh_slices():
    ua = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36"
    req = urllib.request.Request("https://fonts.googleapis.com/css2?family=Noto+Sans+SC&display=swap", headers={"User-Agent": ua})
    css = urllib.request.urlopen(req, timeout=30).read().decode("utf-8")
    blocks = re.findall(r"@font-face\s*{[^}]*?\.(\d+)\.woff2[^}]*?unicode-range:\s*([^;]+);", css)
    if len(blocks) < 50:
        sys.exit("未能解析分片信息")
    lines = [
        '# Chinese unicode-range slices (grouped by character frequency), one per line: "<index> <ranges>".',
        "# Derived from the Google Fonts CSS for Noto Sans SC; only the grouping of code points is reused, no font data.",
        "# Refresh with: scripts/build-fonts.py --refresh-slices",
    ]
    for idx, r in sorted(blocks, key=lambda b: int(b[0])):
        lines.append(idx + " " + ",".join(x.strip().replace("U+", "") for x in r.split(",")))
    with open(SLICES, "w", encoding="utf-8") as f:
        f.write("\n".join(lines) + "\n")
    print(f"已更新 {SLICES}（{len(blocks)} 个分片）", file=sys.stderr)


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--jetbrains")
    p.add_argument("--wenkai-regular")
    p.add_argument("--wenkai-medium")
    p.add_argument("--out")
    p.add_argument("--jobs", type=int, default=os.cpu_count() or 2)
    p.add_argument("--refresh-slices", action="store_true")
    a = p.parse_args()
    if a.refresh_slices:
        return refresh_slices()
    if not all([a.jetbrains, a.wenkai_regular, a.wenkai_medium, a.out]):
        p.error("需要 --jetbrains、--wenkai-regular、--wenkai-medium 与 --out")
    build(a)


if __name__ == "__main__":
    main()
