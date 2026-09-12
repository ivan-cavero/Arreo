import fcntl, os, signal, struct, time
TIOCSCTTY, TIOCGSID = 0x540E, 0x5429
master, slave = os.openpty()
pid = os.fork()
if pid == 0:
    os.setsid()               # own session, but never TIOCSCTTY: the tty has no session
    signal.signal(signal.SIGHUP, signal.SIG_IGN)
    while True:
        os.write(slave, b"tick\n")   # a LIVE child, still writing to the terminal
        time.sleep(0.05)
def state(p):
    try:
        with open(f"/proc/{p}/stat") as f: return f.read().split(") ",1)[1].split()[0]
    except FileNotFoundError: return "gone"
time.sleep(0.4)
print("child state:", state(pid), "(R/S = alive, Z = exited)")
try:
    print("TIOCGSID on MASTER ->", struct.unpack("i", fcntl.ioctl(master, TIOCGSID, b"\0\0\0\0"))[0])
except OSError as e:
    print(f"TIOCGSID on MASTER -> errno {e.errno} ({e.strerror})")
fd = os.open(f"/proc/{pid}/stat", os.O_RDONLY)   # still alive?
os.close(fd)
print("still alive after the ioctl:", state(pid))
print("bytes the live child wrote:", os.read(master, 24).strip())
os.kill(pid, signal.SIGKILL); os.waitpid(pid, 0)
