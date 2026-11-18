import struct
import subprocess
import sys

MAGIC = b'ADX1'
HEADER = struct.Struct('<4sIII')
U32 = struct.Struct('<I')


def request(op, ident, body):
    return HEADER.pack(MAGIC, op, ident, len(body)) + body


def uniform(name, *values):
    return (U32.pack(len(name)) + name + U32.pack(len(values))
            + struct.pack(f'<{len(values)}f', *values))


def draw(ident, program, width, height, red):
    quad = (-1, 1, 0, 0, 0, 0, 1, 1, 1, 0, 1, 0,
            -1, -1, 0, 1, 0, 1, 1, -1, 1, 1, 1, 1)
    header = struct.pack('<4I28fI', program, width, height, 0, *(0,) * 4, *quad, 2)
    return request(2, ident, header + uniform(b'gain', 1) +
                   uniform(b'color', red, 0.25, 0.5, 1) + bytes(8 * 4))


def check(command):
    shader = (b'cbuffer Params : register(b0) { float gain; float4 color; }; '
              b'float4 main() : SV_Target { return gain * color; }')
    requests = b''.join((
        request(1, 1, b'invalid hlsl'),
        request(1, 2, shader),
        draw(3, 2, 4, 4, 0.125),
        request(2, 4, b'truncated'),
        draw(5, 2, 6, 8, 0.75),
        request(3, 6, U32.pack(2)),
        draw(7, 2, 4, 4, 0.125),
    ))
    result = subprocess.run(command, input=requests, stdout=subprocess.PIPE, timeout=30, check=True)
    output = memoryview(result.stdout)
    offset = 0

    def response(op, ident):
        nonlocal offset
        magic, actual_op, actual_id, size = HEADER.unpack_from(output, offset)
        offset += HEADER.size
        assert (magic, actual_op, actual_id) == (MAGIC, op, ident)
        assert 4 <= size <= len(output) - offset
        status, = struct.unpack_from('<i', output, offset)
        payload = output[offset + 4:offset + size].tobytes()
        offset += size
        return status, payload

    assert response(0, 0) == (0, b'')
    status, diagnostic = response(1, 1)
    assert status < 0 and diagnostic
    assert response(1, 2) == (0, b'')
    assert response(2, 3) == (0, bytes((128, 64, 32, 255)) * 16)
    assert response(2, 4)[0] < 0
    assert response(2, 5) == (0, bytes((128, 64, 191, 255)) * 48)
    assert response(3, 6) == (0, b'')
    assert response(2, 7)[0] < 0
    assert offset == len(output)
    print('D3D11 worker passed compile, redraw, malformed request, and release checks')


if __name__ == '__main__':
    check(sys.argv[1:])
