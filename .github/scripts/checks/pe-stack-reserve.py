#!/usr/bin/env python3
"""Prints the stack a Windows executable reserves for its main thread, and checks a minimum.

    pe-stack-reserve.py <file.exe> [--min-bytes N]

Windows starts the main thread with the SizeOfStackReserve of the PE optional header (1 MiB when
the linker was told nothing). Ember's renderer needs more on that thread (#880), so CI reads the
header of the built ember-app.exe instead of trusting the link argument.
"""

import struct
import sys


def stack_reserve(data: bytes) -> int:
    if data[:2] != b"MZ":
        raise ValueError("not an MZ executable")
    (pe_offset,) = struct.unpack_from("<I", data, 0x3C)
    if data[pe_offset:pe_offset + 4] != b"PE\0\0":
        raise ValueError("no PE signature")
    optional = pe_offset + 4 + 20  # after the signature and the COFF file header
    (magic,) = struct.unpack_from("<H", data, optional)
    if magic == 0x20B:  # PE32+
        return struct.unpack_from("<Q", data, optional + 72)[0]
    if magic == 0x10B:  # PE32
        return struct.unpack_from("<I", data, optional + 72)[0]
    raise ValueError(f"unknown optional header magic 0x{magic:x}")


def main(argv: list[str]) -> int:
    if len(argv) not in (2, 4) or (len(argv) == 4 and argv[2] != "--min-bytes"):
        print(__doc__, file=sys.stderr)
        return 2
    with open(argv[1], "rb") as f:
        data = f.read(4096)
    try:
        reserve = stack_reserve(data)
    except (ValueError, struct.error) as e:
        print(f"error: {argv[1]}: {e}", file=sys.stderr)
        return 1
    print(f"{argv[1]}: main thread stack reserve {reserve} bytes ({reserve / 1048576:.1f} MiB)")
    if len(argv) == 4 and reserve < int(argv[3]):
        print(f"error: the reserve is below the required {argv[3]} bytes", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
