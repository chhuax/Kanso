#!/usr/bin/env python3
"""从 macOS 应用图标（1024 px）生成 Windows 用的 .ico（需要 Pillow）。

  python3 tools/gen-windows-icons.py

输出：
- crates/kanso-gpui/windows/kanso.ico      16/24/32/48/64（32 位 DIB）+ 256（PNG），嵌入 exe；
- crates/kanso-gpui/windows/installer.ico  16/24/32/48（32 位 DIB），给 NSIS 安装 / 卸载程序
  （NSIS 对带 PNG 帧的 .ico 兼容性不稳，所以单独一份纯 DIB）。
"""

import io
import struct
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "apple/Kanso/Resources/Assets.xcassets/AppIcon.appiconset/icon_512x512@2x.png"
OUT = ROOT / "crates/kanso-gpui/windows"

src = Image.open(SRC).convert("RGBA")


def dib(n):
    px = src.resize((n, n), Image.LANCZOS).load()
    header = struct.pack("<IiiHHIIiiII", 40, n, n * 2, 1, 32, 0, 0, 0, 0, 0, 0)
    xor = bytearray()
    for y in range(n - 1, -1, -1):  # DIB 自下而上
        for x in range(n):
            r, g, b, a = px[x, y]
            xor += bytes((b, g, r, a))
    and_mask = bytes(((n + 31) // 32 * 4) * n)  # 透明度由 alpha 通道给出，AND 掩码全 0
    return header + bytes(xor) + and_mask


def png(n):
    buf = io.BytesIO()
    src.resize((n, n), Image.LANCZOS).save(buf, "PNG", optimize=True)
    return buf.getvalue()


def write_ico(frames, path):
    out = struct.pack("<HHH", 0, 1, len(frames))
    offset = 6 + 16 * len(frames)
    for n, data in frames:
        size = 0 if n >= 256 else n
        out += struct.pack("<BBBBHHII", size, size, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    path.write_bytes(out + b"".join(d for _, d in frames))


write_ico([(n, dib(n)) for n in (16, 24, 32, 48, 64)] + [(256, png(256))], OUT / "kanso.ico")
write_ico([(n, dib(n)) for n in (16, 24, 32, 48)], OUT / "installer.ico")
