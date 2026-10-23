#!/usr/bin/env python3
"""TEST ONLY: an FTP server that behaves like Aurora's on a modded Xbox 360.
- user xboxftp / xboxftp, root lists drives (Hdd1, Usb0) as folders
- no MLSD / MLST / EPSV / EPRT (passive PASV only), Unix-style LIST
- one connection at a time (max_cons=1), optional --no-size (SIZE not supported)
Usage: fake_aurora.py ROOT PORT [--no-size] [--limit MBPS] [--cons N] [--list-425] [--drives Hdd1,Usb0,Usb1]
"""
import os, sys
from pyftpdlib.authorizers import DummyAuthorizer
from pyftpdlib.handlers import FTPHandler, ThrottledDTPHandler
from pyftpdlib.servers import FTPServer

root, port = sys.argv[1], int(sys.argv[2])
drives = sys.argv[sys.argv.index("--drives") + 1].split(",") if "--drives" in sys.argv else ["Hdd1", "Usb0"]
for d in drives:
    os.makedirs(os.path.join(root, d), exist_ok=True)
auth = DummyAuthorizer()
auth.add_user("xboxftp", "xboxftp", root, perm="elradfmwMT")
drop = {"MLSD", "MLST", "EPSV", "EPRT"} | ({"SIZE"} if "--no-size" in sys.argv else set())
class Aurora(FTPHandler):
    proto_cmds = {k: v for k, v in FTPHandler.proto_cmds.items() if k not in drop}
    banner = "Aurora FTP ready"
    permit_foreign_addresses = False
Aurora.authorizer = auth
if "--list-425" in sys.argv:
    # Overseer R10 HIGH-1 repro: LIST fails with 425 (Xbox servers often refuse data links)
    def ftp_LIST(self, path):
        self.respond("425 Can't open data connection.")
    Aurora.ftp_LIST = ftp_LIST
args = sys.argv[3:]
if "--limit" in args:
    class Slow(ThrottledDTPHandler):
        read_limit = int(float(args[args.index("--limit") + 1]) * 1048576)
    Aurora.dtp_handler = Slow
srv = FTPServer(("127.0.0.1", port), Aurora)
cons = int(args[args.index("--cons") + 1]) if "--cons" in args else 1
srv.max_cons = cons + 1  # pyftpdlib counts its listening socket too
srv.max_cons_per_ip = cons
print("fake aurora on", port, flush=True)
srv.serve_forever()
