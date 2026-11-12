"""A minimal expect-style client for a machine's serial console.

`origami run --background` listens for the console on a loopback TCP port
recorded in state/run.json. Only one client may hold it at a time.
"""
import json
import re
import socket
import time


class Timeout(Exception):
    pass


class Console:
    def __init__(self, machine_dir, log_path=None):
        record = json.load(open(f"{machine_dir}/state/run.json"))
        self.sock = socket.create_connection(("127.0.0.1", record["console_port"]))
        self.sock.settimeout(0.5)
        self.buffer = ""
        self.log = open(log_path, "ab") if log_path else None

    def close(self):
        self.sock.close()
        if self.log:
            self.log.close()

    def _pump(self):
        try:
            data = self.sock.recv(65536)
        except socket.timeout:
            return
        if not data:
            raise EOFError("console closed")
        if self.log:
            self.log.write(data)
            self.log.flush()
        self.buffer += data.decode("latin-1").replace("\r", "")

    def send(self, text):
        self.sock.sendall(text.encode())

    def line(self, text):
        self.send(text + "\r")

    def expect(self, pattern, timeout=120):
        """Wait for a regex; return the match and drop the buffer up to it."""
        rx = re.compile(pattern)
        end = time.monotonic() + timeout
        while True:
            match = rx.search(self.buffer)
            if match:
                self.buffer = self.buffer[match.end():]
                return match
            if time.monotonic() > end:
                raise Timeout(f"waiting for {pattern!r}; tail: {self.buffer[-600:]!r}")
            self._pump()

    def run(self, command, prompt=r"# $", timeout=120):
        """Type a shell command, return its output without the echo."""
        self.buffer = ""
        marker = f"__done_{int(time.time() * 1000) % 100000}__"
        self.line(f"{command}; echo {marker}$?")
        out = self.expect(rf"{marker}(\d+)\n", timeout)
        text = self.buffer_before(out)
        return int(out.group(1)), text

    def buffer_before(self, match):
        return match.string[: match.start()]


def login(con, timeout=1800):
    """Log in as root on the serial console and leave a Bourne shell, so
    `$?` means what the scenarios expect."""
    con.line("")
    con.expect(r"login: ", timeout)
    con.line("root")
    con.expect(r"TERM = \(vt100\) ", 120)
    con.line("")
    con.expect(r"# $", 120)
    con.line("exec /bin/sh")
    time.sleep(2)
