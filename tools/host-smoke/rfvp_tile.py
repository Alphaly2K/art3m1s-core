"""Write an RFVP tile fixture with optional audio and translation requests."""
import argparse
import struct
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("output", type=Path)
parser.add_argument("--services", action="store_true")
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
