# -*- coding: utf-8 -*-
"""生成应用图标（PNG 和 ICO 格式），供 PyInstaller 打包使用。"""

import struct
import zlib
from pathlib import Path


def create_png(width: int, height: int) -> bytes:
    """
    Business Logic（为什么需要这个函数）:
        PyInstaller 和操作系统需要应用图标文件，需要纯 Python 生成
        而不依赖 Pillow 等额外库。

    Code Logic（这个函数做什么）:
        纯 Python 生成 PNG 格式图标：蓝色(#0078D4)圆形背景上白色 "CP" 文字。
        使用基本的像素绘制算法，输出标准 PNG 二进制。
    """
    # 蓝色背景圆形 + 白色 CP 文字
    pixels: list[list[tuple[int, int, int, int]]] = []
    cx: float = width / 2
    cy: float = height / 2
    r: float = min(width, height) / 2 - 2

    # 简单的 5x7 点阵字体 (C 和 P)
    char_c: list[str] = [
        " ###",
        "#   ",
        "#   ",
        "#   ",
        "#   ",
        "#   ",
        " ###",
    ]
    char_p: list[str] = [
        "### ",
        "#  #",
        "#  #",
        "### ",
        "#   ",
        "#   ",
        "#   ",
    ]

    char_w: int = 4
    char_h: int = 7
    total_w: int = char_w * 2 + 1  # C(4) + gap(1) + P(4) = 9
    scale: int = max(1, int(width / 20))
    text_w: int = total_w * scale
    text_h: int = char_h * scale
    text_x0: int = int(cx - text_w / 2)
    text_y0: int = int(cy - text_h / 2)

    for y in range(height):
        row: list[tuple[int, int, int, int]] = []
        for x in range(width):
            dx: float = x - cx
            dy: float = y - cy
            dist: float = (dx * dx + dy * dy) ** 0.5

            if dist <= r:
                # 在圆内 - 检查是否在文字区域
                tx: int = x - text_x0
                ty: int = y - text_y0
                is_text: bool = False

                if 0 <= tx < text_w and 0 <= ty < text_h:
                    char_col: int = tx // scale
                    char_row: int = ty // scale

                    if char_col < char_w and char_row < char_h:
                        # C
                        if char_col < len(char_c[char_row]) and char_c[char_row][char_col] == '#':
                            is_text = True
                    elif char_col >= char_w + 1 and char_row < char_h:
                        # P
                        p_col: int = char_col - char_w - 1
                        if p_col < len(char_p[char_row]) and char_p[char_row][p_col] == '#':
                            is_text = True

                if is_text:
                    row.append((255, 255, 255, 255))  # 白色文字
                elif dist > r - 1.5:
                    # 抗锯齿边缘
                    alpha: int = int(255 * (r - dist + 1.5) / 1.5)
                    alpha = max(0, min(255, alpha))
                    row.append((0, 120, 212, alpha))
                else:
                    row.append((0, 120, 212, 255))  # #0078D4 蓝色
            else:
                row.append((0, 0, 0, 0))  # 透明
        pixels.append(row)

    return _encode_png(width, height, pixels)


def _encode_png(
    width: int,
    height: int,
    pixels: list[list[tuple[int, int, int, int]]],
) -> bytes:
    """
    Business Logic（为什么需要这个函数）:
        将像素数据编码为标准 PNG 格式二进制。

    Code Logic（这个函数做什么）:
        按 PNG 规范写入文件头、IHDR、IDAT（zlib 压缩）和 IEND chunk。
    """
    raw_data: bytearray = bytearray()
    for row in pixels:
        raw_data.append(0)  # filter byte: None
        for r_val, g_val, b_val, a_val in row:
            raw_data.extend([r_val, g_val, b_val, a_val])

    compressed: bytes = zlib.compress(bytes(raw_data))

    def make_chunk(chunk_type: bytes, data: bytes) -> bytes:
        """构造 PNG chunk。"""
        chunk: bytes = chunk_type + data
        return (
            struct.pack(">I", len(data))
            + chunk
            + struct.pack(">I", zlib.crc32(chunk) & 0xFFFFFFFF)
        )

    png: bytearray = bytearray()
    png.extend(b"\x89PNG\r\n\x1a\n")  # PNG signature

    # IHDR
    ihdr_data: bytes = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
    png.extend(make_chunk(b"IHDR", ihdr_data))

    # IDAT
    png.extend(make_chunk(b"IDAT", compressed))

    # IEND
    png.extend(make_chunk(b"IEND", b""))

    return bytes(png)


def png_to_ico(png_data_list: list[tuple[int, bytes]]) -> bytes:
    """
    Business Logic（为什么需要这个函数）:
        Windows 需要 .ico 格式图标，需要将 PNG 数据封装为 ICO 格式。

    Code Logic（这个函数做什么）:
        按 ICO 文件格式规范，写入文件头和图像条目，
        每个条目直接内嵌 PNG 数据。
    """
    num_images: int = len(png_data_list)
    header: bytes = struct.pack("<HHH", 0, 1, num_images)

    entries: bytearray = bytearray()
    data_blocks: bytearray = bytearray()
    offset: int = 6 + num_images * 16  # header(6) + entries(16 each)

    for size, png_data in png_data_list:
        w: int = size if size < 256 else 0
        h: int = size if size < 256 else 0
        entries.extend(
            struct.pack(
                "<BBBBHHII",
                w, h, 0, 0, 1, 32, len(png_data), offset,
            )
        )
        data_blocks.extend(png_data)
        offset += len(png_data)

    return header + bytes(entries) + bytes(data_blocks)


def main() -> None:
    """
    Business Logic（为什么需要这个函数）:
        构建脚本的入口，生成多尺寸图标文件供 PyInstaller 使用。

    Code Logic（这个函数做什么）:
        生成 256x256 PNG（Linux/Mac）和多尺寸 ICO（Windows），
        保存到 scripts/ 目录下。
    """
    scripts_dir: Path = Path(__file__).parent

    # 生成不同尺寸
    sizes: list[int] = [16, 32, 48, 64, 128, 256]
    png_list: list[tuple[int, bytes]] = []

    for size in sizes:
        png_data: bytes = create_png(size, size)
        png_list.append((size, png_data))

    # 保存 256x256 PNG
    png_path: Path = scripts_dir / "icon.png"
    png_path.write_bytes(png_list[-1][1])
    print(f"已生成: {png_path}")

    # 保存 ICO（多尺寸）
    ico_path: Path = scripts_dir / "icon.ico"
    ico_data: bytes = png_to_ico(png_list)
    ico_path.write_bytes(ico_data)
    print(f"已生成: {ico_path}")


if __name__ == "__main__":
    main()
