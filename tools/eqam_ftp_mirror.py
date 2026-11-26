#!/usr/bin/env python3
"""Read-only FTP mirror of the EQAM-420B filesystem (GNU inetutils ftpd on uClinux).
The device has no /bin/ls, so LIST is empty; NLST works. Directories are detected by CWD.
Only NLST/CWD/SIZE/RETR are issued -- never STOR/DELE/RNFR/MKD/SITE.
/dev and /sys are listed but never read (opening e.g. /dev/watchdog can have side effects).
From /proc only a fixed allowlist of files is fetched.
Usage: EQAM_FTP_PW=... python3 -I tools/eqam_ftp_mirror.py OUTDIR [user]
       (typically: EQAM_FTP_PW=$(rbw get 10.42.0.184 | head -n1) ...)"""
import ftplib, os, sys, time, posixpath

HOST = os.environ.get("EQAM_HOST", "10.42.0.184")
OUT = sys.argv[1]
USER = sys.argv[2] if len(sys.argv) > 2 else "Admin"
DELAY = float(os.environ.get("EQAM_DELAY", "1.5"))
MAXSIZE = 8 * 1024 * 1024
LIST_ONLY = ("/dev", "/sys", "/proc")
PROC_FILES = ["version", "cpuinfo", "meminfo", "mtd", "mounts", "partitions", "interrupts",
              "uptime", "loadavg", "cmdline", "devices", "filesystems", "modules", "iomem",
              "net/dev", "net/route", "net/arp", "net/igmp", "net/dev_mcast", "net/tcp",
              "net/udp", "net/snmp", "stat", "buddyinfo", "slabinfo", "vmstat", "locks",
              "misc", "ioports", "sysvipc/shm", "sysvipc/msg", "sysvipc/sem"]

f = None
nxfer = 0
def reconnect():
    """ftpd starts failing PASV with 425 EADDRNOTAVAIL after a few dozen data
    connections on one control session; a fresh login clears it."""
    global f, nxfer
    if f is not None:
        try: f.quit()
        except Exception: pass
    time.sleep(3)
    for attempt in range(10):
        try:
            f = ftplib.FTP(); f.connect(HOST, 21, timeout=30); break
        except ConnectionRefusedError:
            # inetd stops accepting ftp for a while after many rapid connections
            time.sleep(60)
    f.login(USER, os.environ.get("EQAM_FTP_PW", "anonymous@"))
    f.sendcmd("TYPE I"); nxfer = 0
reconnect()
index = open(os.path.join(OUT, "INDEX.txt"), "w")  # rewritten each run; files resume

def nlst(d):
    for attempt in range(6):
        try: return f.nlst(d)
        except ftplib.error_perm: return []
        except (ftplib.error_temp, ConnectionError, EOFError, OSError):
            time.sleep(5 * (attempt + 1)); reconnect()
    return []

def isdir(p):
    try:
        f.cwd(p); f.cwd("/"); return True
    except ftplib.error_perm:
        return False

def fetch(p):
    global nxfer
    if nxfer >= 30: reconnect()
    try:
        size = int(f.sendcmd("SIZE " + p).split()[1])
    except Exception as e:
        index.write(f"F ? {p} (SIZE failed: {e})\n"); return
    index.write(f"F {size} {p}\n"); index.flush()
    if size > MAXSIZE: return
    dst = os.path.join(OUT, "fs" + p)
    if os.path.exists(dst) and os.path.getsize(dst) > 0:
        return  # resume. NB: this ftpd's SIZE over-reports text files (counts LF as CRLF)
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    for attempt in range(6):
        try:
            nxfer += 1
            with open(dst, "wb") as fh:
                f.retrbinary("RETR " + p, fh.write)
            break
        except ftplib.error_temp as e:
            index.write(f"  RETR retry {attempt}: {e}\n"); time.sleep(5 * (attempt + 1)); reconnect()
        except ConnectionRefusedError as e:
            index.write(f"  RETR retry {attempt}: {e}\n"); reconnect()
        except Exception as e:
            index.write(f"  RETR failed: {e}\n"); break
    time.sleep(DELAY)

def walk(d, depth=0):
    global nxfer
    nxfer += 1
    if nxfer >= 30: reconnect()
    names = nlst(d)
    time.sleep(DELAY)
    for n in sorted(names):
        p = posixpath.normpath("/" + n.lstrip("/"))
        if p == d or not p.startswith(d.rstrip("/") + "/"):
            p = posixpath.join(d, posixpath.basename(n))
        if isdir(p):
            index.write(f"D {p}\n"); index.flush()
            if any(p == x or p.startswith(x + "/") for x in LIST_ONLY):
                if depth < 2 and p.startswith(("/dev", "/sys")):
                    walk_list_only(p, depth + 1)
                continue
            walk(p, depth + 1)
        else:
            if any(p.startswith(x + "/") for x in LIST_ONLY):
                index.write(f"L {p}\n")
            else:
                fetch(p)

def walk_list_only(d, depth):
    names = nlst(d)
    for n in sorted(names): index.write(f"L {n}\n")

walk("/")
for pf in PROC_FILES:
    fetch("/proc/" + pf)
f.quit(); index.close()
