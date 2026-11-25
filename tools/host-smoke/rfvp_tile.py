"""Write an RFVP tile fixture with optional audio, translation, and IO."""
import argparse
import struct
import zlib
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("output", type=Path)
parser.add_argument("--services", action="store_true")
parser.add_argument("--io", action="store_true")
options = parser.parse_args()

calls = [
    (b"PrimSetTile", (1, 2, 8, 12, 16, 20)),
    (b"PrimSetDraw", (1, 1)),
    (b"PrimGroupIn", (1, 0)),
]
if options.services:
    calls += [
        (b"SoundMasterVol", (37,)),
        (b"TextBuff", (0, 120, 32)),
        (b"TextPrint", (0, b"RFVP smoke text")),
    ]
if options.io:
    def texture(bgra):
        pixels = bytes(bgra) * 16
        header = struct.pack("<4s8H3I", b"NVSG", 0, 1, 4, 4, 0, 0, 0, 0, 1, 0, 0)
        return struct.pack("<4sII", b"hzc1", len(pixels), len(header)) + header + zlib.compress(pixels)

    folder = options.output.parent
    names = b"packed.hzc\0"
    red = texture((0, 0, 255, 255))
    (folder / "graph.bin").write_bytes(
        struct.pack("<IIIII", 1, len(names), 0, 20 + len(names), len(red)) + names + red
    )
    (folder / "loose.hzc").write_bytes(texture((0, 255, 0, 255)))
    calls += [
        (b"GraphLoad", (3, b"graph/packed.hzc")),
        (b"GraphLoad", (4, b"loose.hzc")),
        (b"PrimSetSprt", (2, 3, 40, 20)),
        (b"PrimSetDraw", (2, 1)),
        (b"PrimGroupIn", (2, 0)),
        (b"PrimSetSprt", (3, 4, 50, 20)),
        (b"PrimSetDraw", (3, 1)),
        (b"PrimGroupIn", (3, 0)),
        (b"SaveData", (0, 0, 0)),
        (b"SaveData", (4, 9, 0)),
        (b"SaveCreate", (0, b"Host IO smoke")),
        (b"SaveCreate", (3, 7)),
        (b"SaveData", (13, 7, 45)),
        (b"SaveData", (3, 7, 8)),
        (b"SaveData", (2, 8, 0)),
        (b"Load", (7,)),
    ]
calls.append((b"ThreadSleep", (100,)))
script = bytearray(4)
for index, (_, args) in enumerate(calls):
    for arg in args:
        if isinstance(arg, bytes):
            script += bytes((14, len(arg) + 1)) + arg + b"\0"  # PushString
        else:
            script += bytes((12, arg))  # PushI8
    script += struct.pack("<BH", 3, index)  # Syscall
script += struct.pack("<BI", 6, len(script) - 5)  # Jump back to the sleep
struct.pack_into("<I", script, 0, len(script))
script += struct.pack("<IHHBBBBH", 4, 0, 0, 0, 0, 1, 0, len(calls))
for name, args in calls:
    script += bytes((len(args), len(name) + 1)) + name + b"\0"
script += b"\0\0"
options.output.write_bytes(script)
