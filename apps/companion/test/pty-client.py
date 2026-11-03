# A herdr TUI client in a pseudo-terminal with a chosen grid and pixel size (TIOCSWINSZ
# ws_xpixel/ws_ypixel), for the dock e2e (e2e-placements.mjs). No terminal emulator: the
# client's output is read and dropped; the few queries it waits for are answered (device
# attributes), the rest time out as on a plain terminal.
#
#   python3 -I pty-client.py <cols> <rows> <xpx> <ypx> <commands file> <herdr> [args...]
#
# The commands file is polled: each new line is hex of the bytes to type (`02 42` is ^B B),
# or `quit`. The client's exit ends the driver.
import os, pty, sys, fcntl, termios, struct, select, time

cols, rows, xpx, ypx = map(int, sys.argv[1:5])
commands = sys.argv[5]
argv = sys.argv[6:]

pid, fd = pty.fork()
if pid == 0:
    fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack('HHHH', rows, cols, xpx, ypx))
    os.environ['TERM'] = 'xterm-256color'
    os.execvp(argv[0], argv)

fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack('HHHH', rows, cols, xpx, ypx))
done = 0
buf = b''
deadline = time.time() + 600
while time.time() < deadline:
    r, _, _ = select.select([fd], [], [], 0.05)
    if fd in r:
        try:
            data = os.read(fd, 65536)
        except OSError:
            break
        if not data:
            break
        buf = (buf + data)[-4096:]
        # primary device attributes: a VT220-ish answer, so the client stops waiting
        if b'\x1b[c' in buf or b'\x1b[0c' in buf:
            os.write(fd, b'\x1b[?62;22c')
            buf = buf.replace(b'\x1b[c', b'').replace(b'\x1b[0c', b'')
    try:
        with open(commands) as f:
            lines = f.read().splitlines()
    except OSError:
        lines = []
    for line in lines[done:]:
        done += 1
        line = line.strip()
        if line == 'quit':
            deadline = 0
            break
        if line:
            os.write(fd, bytes.fromhex(line))
    try:
        wpid, _ = os.waitpid(pid, os.WNOHANG)
        if wpid:
            break
    except ChildProcessError:
        break
try:
    os.close(fd)
except OSError:
    pass
