#!/usr/bin/env python3
"""Minimal telnet client for the EQAM-420B (uClinux login). Logs in and runs a fixed
list of commands, printing a transcript. The password is taken from $EQAM_TELNET_PW and
is never echoed into the transcript.
Usage: EQAM_TELNET_PW=$(rbw get 10.42.0.184 | head -n1) python3 -I tools/eqam_telnet.py USER 'cmd1' 'cmd2' ...
Only pass commands you know to be read-only."""
import os, socket, sys, time

HOST = os.environ.get("EQAM_HOST", "10.42.0.184")
s = socket.create_connection((HOST, 23), timeout=10); s.settimeout(0.5)

def read(wait=2.0, until=None):
    buf = b""; end = time.time() + wait
    while time.time() < end:
        try: d = s.recv(4096)
        except socket.timeout: d = None
        if d == b"": break
        if not d:
            if until and until in buf: break
            continue
        i = 0; out = b""
        while i < len(d):
            if d[i] == 255 and i + 2 < len(d) and d[i+1] in (251, 252, 253, 254):
                cmd, opt = d[i+1], d[i+2]
                if cmd == 251 and opt in (1, 3):   # accept server ECHO / SGA
                    out += bytes([255, 253, opt])
                elif cmd in (251, 252): out += bytes([255, 254, opt])
                else: out += bytes([255, 252, opt])
                i += 3
            elif d[i] == 255 and i + 1 < len(d):
                i += 2
            else:
                buf += d[i:i+1]; i += 1
        if out: s.sendall(out)
        end = max(end, time.time() + 0.7)  # extend while data flows
    return buf.decode("latin-1")

def send(line): s.sendall(line.encode() + b"\r\n")

sys.stdout.write(read(4, until=b"login:")); send(sys.argv[1]); sys.stdout.write(sys.argv[1] + "\n")
sys.stdout.write(read(4, until=b"assword:")); send(os.environ["EQAM_TELNET_PW"]); sys.stdout.write("<password redacted>\n")
sys.stdout.write(read(5))
for c in sys.argv[2:]:
    send(c); sys.stdout.write(read(float(os.environ.get("EQAM_CMD_WAIT", "3"))))
    sys.stdout.flush()
send("exit"); sys.stdout.write(read(2))
