#!/usr/bin/env python3
"""Rebuild the HUD atlas using Pillow and Noto Sans CJK JP Medium 2.004.

Example: python3 assets/hud-glyphs.py /usr/share/fonts/opentype/noto/NotoSansCJK-Medium.ttc
The atlas itself is distributed under the SIL Open Font License 1.1; see
LICENSES/hud-font-OFL.txt. Neither Pillow nor a system font is needed to run the game.
"""

import argparse
import struct
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

SIZES = (12, 13, 15)
FEEDBACK = (
    "セーブデータをコピーしました コピーできませんでした セーブデータを読み込めませんでした "
    "セーブデータを貼り付けました クリップボードを利用できません 変更しました 実行できませんでした"
)


def write_atlas(path, face):
    root = Path(__file__).resolve().parents[1]
    hud = (root / "src/client/hud.rs").read_text(encoding="utf-8")
    chars = sorted(set(map(chr, range(32, 127))) | {c for c in hud + FEEDBACK if ord(c) > 127})
    directory = bytearray()
    glyphs = bytearray()
    for size in SIZES:
        font = ImageFont.truetype(path, size, index=face)
        assert font.getname() == ("Noto Sans CJK JP", "Medium"), font.getname()
        directory.extend(struct.pack("<I", 4 * len(SIZES) + len(glyphs)))
        glyphs.extend(rasterize(font, chars, max(12, size - 1)))
    output = root / "assets/hud-glyphs.bin"
    temporary = output.with_suffix(".tmp")
    temporary.write_bytes(directory + glyphs)
    temporary.replace(output)


def rasterize(font, chars, baseline):
    entries, runs = bytearray(), bytearray()
    notdef = bytes(font.getmask(chr(0x10FFFF), mode="L"))
    for char in chars:
        assert bytes(font.getmask(char, mode="L")) != notdef, char
        image = Image.new("L", (16, 20))
        left, top, right, bottom = font.getbbox(char, anchor="ls")
        assert -1 <= left and right <= 15 and top >= -baseline and bottom + baseline <= 20, char
        ImageDraw.Draw(image).text((1, baseline), char, 255, font, anchor="ls")
        offset = len(runs) // 2
        pix = image.load()
        for y in range(image.height):
            row = [(pix[x, y] * 3 + 127) // 255 for x in range(16)]
            x = 0
            while x < 16:
                alpha = row[x]
                end = x + 1
                while end < 16 and row[end] == alpha:
                    end += 1
                if alpha:
                    runs.extend(struct.pack("<H", x | y << 4 | (end - x - 1) << 9 | alpha << 13))
                x = end
        assert len(runs) // 2 > offset or char.isspace(), char
        entries.extend(struct.pack("<4H", ord(char), round(font.getlength(char) * 64), offset,
                                   len(runs) // 2 - offset))
    return struct.pack("<H", len(chars)) + entries + runs


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("font", help="NotoSansCJK-Medium.ttc or a standalone Japanese face")
    parser.add_argument("--face", type=int, default=0, help="JP face index in the supplied collection")
    args = parser.parse_args()
    write_atlas(args.font, args.face)
