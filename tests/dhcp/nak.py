#!/usr/bin/env python3
"""Send a raw DHCPREQUEST (INIT-REBOOT style: option 50 + 54, no ciaddr) for
the given address from the given MAC on interface c0 and print the message
type of the first matching reply. Used by run.sh to exercise the NAK path,
which busybox udhcpc never triggers (its -r sends a DISCOVER)."""
import socket
import struct
import sys

mac = bytes.fromhex(sys.argv[1].replace(":", ""))
want = bytes(int(x) for x in sys.argv[2].split("."))
server = bytes(int(x) for x in sys.argv[3].split("."))
iface = sys.argv[4] if len(sys.argv) > 4 else "c0"

s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
s.setsockopt(socket.SOL_SOCKET, 25, iface.encode() + b"\0")  # SO_BINDTODEVICE
s.bind(("0.0.0.0", 68))
s.settimeout(3)

xid = b"\x12\x34\x56\x78"
pkt = bytearray(240)
pkt[0:4] = b"\x01\x01\x06\x00"
pkt[4:8] = xid
pkt[10:12] = b"\x80\x00"  # broadcast flag
pkt[28:34] = mac
pkt[236:240] = b"\x63\x82\x53\x63"
pkt += b"\x35\x01\x03"  # REQUEST
pkt += b"\x32\x04" + want  # requested address
pkt += b"\x36\x04" + server  # server id
pkt += b"\xff"
s.sendto(bytes(pkt), ("255.255.255.255", 67))

names = {2: "OFFER", 5: "ACK", 6: "NAK"}
try:
    while True:
        d, _ = s.recvfrom(1500)
        if d[4:8] != xid:
            continue
        i = 240
        while i < len(d) and d[i] != 255:
            if d[i] == 0:
                i += 1
                continue
            code, ln = d[i], d[i + 1]
            if code == 53:
                print(names.get(d[i + 2], str(d[i + 2])))
                sys.exit(0)
            i += 2 + ln
except socket.timeout:
    print("TIMEOUT")
    sys.exit(1)
