# PortPilot 图标生成:多尺寸 ICO(16/24/32/48/64/128/256)
# 设计:深色圆角方块底 + 青色"串口波形"(正弦) + 白色定位针点(领航意象)
# 运行:python scripts/make_icon.py(纯标准库,无 PIL)
import math
import os
import struct

SIZES = [16, 24, 32, 48, 64, 128, 256]


def clamp(v):
    return max(0, min(255, int(v)))


def render(size):
    """返回 px[y][x] = (r,g,b,a),自底向上由调用方处理"""
    px = [[(0, 0, 0, 0)] * size for _ in range(size)]
    dark = (22, 25, 30, 255)
    r = 48 * size / 256.0

    def inside_rounded(x, y):
        if x < r and y < r:
            return (x - r) ** 2 + (y - r) ** 2 <= r * r
        if x > size - r and y < r:
            return (x - (size - r)) ** 2 + (y - r) ** 2 <= r * r
        if x < r and y > size - r:
            return (x - r) ** 2 + (y - (size - r)) ** 2 <= r * r
        if x > size - r and y > size - r:
            return (x - (size - r)) ** 2 + (y - (size - r)) ** 2 <= r * r
        return True

    cy = size * 0.5
    amp = size * 28 / 256.0
    freq = 2.2 * math.pi / size

    accent = (76, 194, 255, 255)
    white = (240, 244, 248, 255)

    for y in range(size):
        for x in range(size):
            if not inside_rounded(x, y):
                continue
            col = dark
            wy = cy - amp * math.sin(x / size * freq * size) \
                - amp * 0.3 * math.sin(x / size * freq * size * 3)
            d = abs(y - wy)
            if d < size * 10 / 256.0:
                t = 1.0 - d / (size * 10 / 256.0)
                col = (accent[0], accent[1], accent[2],
                       clamp(255 * (0.45 + 0.55 * t)))
            elif d < size * 16 / 256.0:
                col = (40, 70, 95, 255)
            px_x, px_y = size * 0.78, size * 0.78
            if (x - px_x) ** 2 + (y - px_y) ** 2 <= (size * 14 / 256.0) ** 2:
                col = white
            px[y][x] = col
    return px


def ico_image(size, px):
    w = h = size
    xor = bytearray()
    for y in reversed(range(h)):
        for x in range(w):
            r, g, bl, a = px[y][x]
            xor += struct.pack("BBBB", bl, g, r, a)
    and_row = ((w + 31) // 32) * 4
    and_mask = bytes(and_row * h)
    img = struct.pack("<IiiHHIIiiII", 40, w, h * 2, 1, 32, 0,
                      len(xor) + len(and_mask), 0, 0, 0, 0) + bytes(xor) + and_mask
    return img


def main():
    out = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)),
                                       "..", "src", "app", "src-tauri", "icons", "icon.ico"))
    images = []
    for size in SIZES:
        px = render(size)
        images.append(ico_image(size, px))
    header = struct.pack("<HHH", 0, 1, len(SIZES))
    entries = b""
    data_off = 6 + 16 * len(SIZES)
    for size, img in zip(SIZES, images):
        entries += struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0,
                               1, 32, len(img), data_off)
        data_off += len(img)
    with open(out, "wb") as f:
        f.write(header)
        f.write(entries)
        for img in images:
            f.write(img)
    print(f"icon.ico 已生成: {len(SIZES)} 尺寸, 总 {os.path.getsize(out)}B")


if __name__ == "__main__":
    main()
