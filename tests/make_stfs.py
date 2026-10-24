# Synthetic Xbox 360 STFS header for tests: make_stfs.py OUT MAGIC CONTENT_TYPE_HEX TITLEID_HEX NAME [EXTRA]
import sys, struct
out, magic, ct, tid, name = sys.argv[1:6]
extra = (sys.argv[6] if len(sys.argv) > 6 else "").encode()
b = bytearray(0xA000)
b[0:4] = magic.ljust(4).encode()
b[0x344:0x348] = struct.pack(">I", int(ct, 16))
b[0x360:0x364] = struct.pack(">I", int(tid, 16))
n = name.encode("utf-16-be")[:0x7e]
b[0x411:0x411 + len(n)] = n
open(out, "wb").write(bytes(b) + extra)
