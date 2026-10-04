#!/usr/bin/env python3
"""生成版本目录：把 scripts/package.sh 产出的安装包整理成一个版本的 GitHub Release 附件。

    python3 scripts/release-info.py --channel stable [--min-supported 0.1.0]

更新说明必须中英文都有：默认读取 release-notes/{版本}.zh-CN.md 与 release-notes/{版本}.en-US.md，
任何一份缺失或为空都会失败。

产物 dist/release/{版本}/：安装包与桌面安装程序（硬链接/复制自 dist/）、release.json、SHA256SUMS。
整个目录作为 GitHub Release（标签 client-v{版本}）发布；看看服务端按 release.json 同步安装包、提供检查更新与下载。
"""

import argparse
import datetime
import hashlib
import json
import os
import re
import shutil
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TARGETS = ["linux-x86_64", "linux-aarch64", "windows-x86_64", "darwin-x86_64", "darwin-aarch64"]
SEMVER = re.compile(r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$")


def cargo_version() -> str:
    m = re.search(r'^version = "([^"]+)"', (ROOT / "Cargo.toml").read_text(), re.M)
    if not m:
        sys.exit("Cargo.toml 中没有 version")
    return m.group(1)


def notes(path: Path) -> str:
    """读取一份更新说明；缺失或为空时退出（中英文都必须有）。"""
    if not path.is_file():
        sys.exit(f"缺少更新说明 {path}（中英文两份都必须有）")
    text = path.read_text(encoding="utf-8").strip()
    if not text:
        sys.exit(f"更新说明 {path} 为空")
    if "<!-- draft" in text:
        sys.exit(f"更新说明 {path} 还是草稿，改好后删掉 <!-- draft … --> 注释")
    return text


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--version", default=None, help="默认取 Cargo.toml")
    ap.add_argument("--channel", default="stable", choices=["stable", "beta"])
    ap.add_argument("--min-supported", default="", help="最低支持版本：低于它的客户端被要求更新且不能登录")
    ap.add_argument("--notes-dir", default=str(ROOT / "release-notes"), help="更新说明目录（{版本}.zh-CN.md / {版本}.en-US.md）")
    ap.add_argument("--dist", default=str(ROOT / "dist"))
    args = ap.parse_args()

    version = args.version or cargo_version()
    if not SEMVER.match(version):
        sys.exit(f"版本号不是 semver：{version}")
    if args.min_supported and not SEMVER.match(args.min_supported):
        sys.exit(f"最低支持版本不是 semver：{args.min_supported}")
    notes_dir = Path(args.notes_dir)
    notes_zh = notes(notes_dir / f"{version}.zh-CN.md")
    notes_en = notes(notes_dir / f"{version}.en-US.md")

    dist = Path(args.dist)
    out = dist / "release" / version
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True)

    packages = []
    # 压缩包（自动更新与 Linux 安装用）+ Windows / macOS 桌面安装程序（package 为 exe / dmg，客户端更新不会选它们）
    name_re = re.compile(rf"^looklook-{re.escape(version)}-({'|'.join(TARGETS)})(?:\.(tar\.gz|zip|dmg)|-setup\.(exe))$")
    for f in sorted(dist.iterdir()):
        m = name_re.match(f.name)
        if not m or not f.is_file():
            continue
        dst = out / f.name
        try:
            os.link(f, dst)
        except OSError:
            shutil.copy2(f, dst)
        packages.append({
            "target": m.group(1),
            "package": m.group(2) or m.group(3),
            "file": f.name,
            "sha256": sha256(dst),
            "size_bytes": dst.stat().st_size,
        })
    if not packages:
        sys.exit(f"{dist} 中没有 looklook-{version}-*.tar.gz / .zip，先执行 scripts/package.sh")

    info = {
        "version": version,
        "channel": args.channel,
        "published_at": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "min_supported_version": args.min_supported or None,
        "notes_zh": notes_zh,
        "notes_en": notes_en,
        "packages": packages,
    }
    (out / "release.json").write_text(json.dumps(info, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    sums = [f"{p['sha256']}  {p['file']}" for p in packages] + [f"{sha256(out / 'release.json')}  release.json"]
    (out / "SHA256SUMS").write_text("\n".join(sums) + "\n")

    for p in packages:
        print(f"  {p['target']:<16} {p['package']:<7} {p['size_bytes'] / 1048576:6.1f} MB  {p['file']}", file=sys.stderr)
    print(out)


if __name__ == "__main__":
    main()
