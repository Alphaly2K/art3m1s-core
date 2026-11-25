"""Write a standalone RFVP script that draws one white tile."""
import struct
import sys
from pathlib import Path

calls = [
    (b"PrimSetTile", (1, 2, 8, 12, 16, 20)),
    (b"PrimSetDraw", (1, 1)),
    (b"PrimGroupIn", (1, 0)),
    (b"ThreadSleep", (100,)),
]
script = bytearray(4)
for index, (_, args) in enumerate(calls):
    for arg in args:
        script += bytes((12, arg))  # PushI8
    script += struct.pack("<BH", 3, index)  # Syscall
script += struct.pack("<BI", 6, len(script) - 5)  # Jump back to the sleep
struct.pack_into("<I", script, 0, len(script))
script += struct.pack("<IHHBBBBH", 4, 0, 0, 0, 0, 1, 0, len(calls))
for name, args in calls:
    script += bytes((len(args), len(name) + 1)) + name + b"\0"
script += b"\0\0"
Path(sys.argv[1]).write_bytes(script)
