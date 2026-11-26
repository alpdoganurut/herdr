"""Fork (remote browser), I1: the `ssh` of the end-to-end test.

The remote machine is a second herdr server on this Mac, in a directory of
its own. This stands in for OpenSSH:

- a remote command runs under the remote machine's environment (`b-env.json`)
  with its input and output relayed; while the file `drop` exists, a running
  one ends (its command's input closes) and a new one fails at once;
- `ControlMaster=yes` starts a master: it serves its control socket until its
  input ends, and its forwards are unix sockets that dial on the remote
  machine's side (`dialmap.json` maps a `host:port` to a local port, so the
  remote machine's localhost is another port than this Mac's; every dial is
  logged in `dials.log`);
- `-O check|forward|cancel|exit` asks the master on `-S`, and answers in
  OpenSSH's words;
- `-G` prints the control path.

Nothing listens on TCP and nothing is killed: everything ends with its input
or its control socket.
"""

import json
import os
import socket
import subprocess
import sys
import threading
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def args():
    control = operation = forward = None
    master = configuration = False
    previous = None
    last = None
    for arg in sys.argv[1:]:
        if previous == "-S":
            control = arg
        if previous == "-O":
            operation = arg
        if previous == "-L":
            forward = arg
        if arg == "ControlMaster=yes":
            master = True
        if arg == "-G":
            configuration = True
        previous = arg
        last = arg
    return control, operation, forward, master, configuration, last


def log(name, line):
    with open(os.path.join(ROOT, name), "a") as out:
        out.write(line + "\n")


def pipe(src, dst):
    try:
        while True:
            data = src.recv(65536)
            if not data:
                break
            dst.sendall(data)
    except OSError:
        pass
    for end in (dst,):
        try:
            end.shutdown(socket.SHUT_WR)
        except OSError:
            pass


def dial(host, port):
    try:
        with open(os.path.join(ROOT, "dialmap.json")) as f:
            mapped = json.load(f)
    except OSError:
        mapped = {}
    log("dials.log", f"{host}:{port}")
    target = mapped.get(f"{host}:{port}")
    if target is None:
        return None
    return socket.create_connection(("127.0.0.1", int(target)), timeout=10)


class Master:
    def __init__(self, control):
        self.control = control
        self.forwards = {}
        self.lock = threading.Lock()

    def add(self, spec):
        sock, rest = spec.split(":", 1)
        host, port = rest.rsplit(":", 1)
        listener = socket.socket(socket.AF_UNIX)
        old = os.umask(0o177)
        try:
            listener.bind(sock)
        finally:
            os.umask(old)
        listener.listen(64)
        with self.lock:
            self.forwards[spec] = listener
        threading.Thread(target=self.accept, args=(listener, host.strip("[]"), int(port)), daemon=True).start()

    def accept(self, listener, host, port):
        while True:
            try:
                near, _ = listener.accept()
            except OSError:
                return
            far = dial(host, port)
            if far is None:
                near.close()
                continue
            threading.Thread(target=pipe, args=(near, far), daemon=True).start()
            threading.Thread(target=pipe, args=(far, near), daemon=True).start()

    def cancel(self, spec):
        with self.lock:
            listener = self.forwards.pop(spec, None)
        if listener is None:
            return False
        listener.close()
        return True

    def handle(self, conn):
        request = json.loads(conn.makefile().readline() or "{}")
        op = request.get("op")
        reply = {"err": "", "code": 0}
        if op == "check":
            reply["err"] = f"Master running (pid={os.getpid()})\r\n"
        elif op == "forward":
            try:
                self.add(request["forward"])
            except OSError as error:
                reply = {"err": f"mux_client_forward: forwarding request failed: {error}\r\n", "code": 255}
        elif op == "cancel":
            if not self.cancel(request["forward"]):
                reply = {"err": "mux_client_forward: forwarding request failed: port not forwarded\r\n", "code": 255}
        elif op == "exit":
            reply["err"] = "Exit request sent.\r\n"
        conn.sendall((json.dumps(reply) + "\n").encode())
        conn.close()
        if op == "exit":
            self.end()

    def end(self):
        try:
            os.unlink(self.control)
        except OSError:
            pass
        os._exit(0)

    def serve(self):
        server = socket.socket(socket.AF_UNIX)
        old = os.umask(0o177)
        try:
            server.bind(self.control)
        finally:
            os.umask(old)
        server.listen(16)

        def loop():
            while True:
                try:
                    conn, _ = server.accept()
                except OSError:
                    return
                threading.Thread(target=self.handle, args=(conn,), daemon=True).start()

        threading.Thread(target=loop, daemon=True).start()
        # the master lives until its input ends
        while sys.stdin.buffer.read(4096):
            pass
        self.end()


def control_op(control, operation, forward):
    client = socket.socket(socket.AF_UNIX)
    try:
        client.connect(control)
    except OSError:
        sys.stderr.write(f"Control socket connect({control}): No such file or directory\r\n")
        return 255
    client.sendall((json.dumps({"op": operation, "forward": forward}) + "\n").encode())
    reply = json.loads(client.makefile().readline() or '{"err": "", "code": 255}')
    sys.stderr.write(reply["err"])
    return reply["code"]


def remote(command):
    drop = os.path.join(ROOT, "drop")
    if os.path.exists(drop):
        sys.stderr.write("ssh: connect to host: Connection refused\r\n")
        return 255
    with open(os.path.join(ROOT, "b-env.json")) as f:
        env = json.load(f)
    log("remote.log", command)
    child = subprocess.Popen(["/bin/sh", "-c", command], stdin=subprocess.PIPE, stdout=subprocess.PIPE, env=env, cwd=env["HOME"])

    def feed():
        try:
            while True:
                data = os.read(0, 65536)
                if not data:
                    break
                child.stdin.write(data)
                child.stdin.flush()
        except OSError:
            pass
        try:
            child.stdin.close()
        except OSError:
            pass

    def drain():
        try:
            while True:
                data = child.stdout.read1(65536)
                if not data:
                    break
                os.write(1, data)
        except OSError:
            pass

    threading.Thread(target=feed, daemon=True).start()
    out = threading.Thread(target=drain, daemon=True)
    out.start()
    while child.poll() is None:
        if os.path.exists(drop):
            # the connection drops: the remote side reads the end of its input
            try:
                child.stdin.close()
            except OSError:
                pass
            deadline = time.time() + 5
            while child.poll() is None and time.time() < deadline:
                time.sleep(0.05)
            return 255
        time.sleep(0.1)
    out.join(timeout=2)
    return child.returncode


def main():
    control, operation, forward, master, configuration, last = args()
    if configuration:
        print(f"controlpath {control or ''}")
        return 0
    if operation:
        return control_op(control, operation, forward)
    if master:
        log("masters.log", control)
        Master(control).serve()
        return 0
    return remote(last)


if __name__ == "__main__":
    sys.exit(main())
