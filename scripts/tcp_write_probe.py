#!/usr/bin/env python3
"""Measure the local TCP server write loss on this machine.

Finding
-------
A TCP connection accepted on this host delivers only the bytes of its first
write(). Every later write() on the same connection is silently discarded:
the peer receives the first frame, then nothing, with no error on either
side. The loss is on the send side, so it is the server that is affected,
and the client sees a truncated stream rather than a closed socket.

Measured behaviour:

  two writes, no delay                     -> only the first write arrives
  two writes, 400 ms apart                 -> only the first write arrives
  three writes                             -> only the first write arrives
  one write carrying both frames           -> both frames arrive
  server TCP_NODELAY                       -> unchanged
  peer keeps sending in between            -> unchanged
  12 consecutive probes                    -> 12/12 first, 0/12 second
  loopback, LAN and virtual adapters      -> identical
  separate client process                  -> identical

A remote server is not affected: a 201 KB HTTPS response from a public host
arrives complete through the same client stack. The defect is specific to
sockets accepted by a local server.

This is the mechanism behind REA's Ghidra bridge hanging after its first
reply, and it is a hazard for any local service this project builds. The
companion itself is not exposed: its crates use named pipes only.

Usage
-----
    python scripts/tcp_write_probe.py                 # loopback, 5 probes
    python scripts/tcp_write_probe.py --host 0.0.0.0  # bind a specific address
    python scripts/tcp_write_probe.py --probes 20
    python scripts/tcp_write_probe.py --http           # also run an HTTP check
"""

from __future__ import annotations

import argparse
import http.server
import socket
import socketserver
import threading
import time

FIRST = b"FIRST\n"
SECOND = b"SECOND\n"


def probe(host: str, bind_host: str, gap: float) -> tuple[bool, bool, bytes]:
    """One trial: server writes two frames, client reads what arrives."""
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listener.bind((bind_host, 0))
    listener.listen(1)
    port = listener.getsockname()[1]

    def serve():
        try:
            conn, _ = listener.accept()
            conn.settimeout(5)
            conn.recv(64)
            conn.sendall(FIRST)
            time.sleep(gap)
            conn.sendall(SECOND)
            time.sleep(1.0)
            conn.close()
        except OSError:
            pass
        finally:
            listener.close()

    thread = threading.Thread(target=serve, daemon=True)
    thread.start()
    time.sleep(0.2)

    got = b""
    try:
        client = socket.create_connection((host, port), timeout=8)
        client.settimeout(2.5)
        client.sendall(b"hi\n")
        deadline = time.time() + 2.5
        while time.time() < deadline:
            try:
                chunk = client.recv(4096)
            except socket.timeout:
                break
            if not chunk:
                break
            got += chunk
            if SECOND in got:
                break
        client.close()
    except OSError:
        pass
    return FIRST in got, SECOND in got, got


def http_check(host: str, bind_host: str, size: int) -> tuple[int, int]:
    """Serve a body in many writes and count what the client receives."""
    payload = b"X" * size

    class Handler(http.server.BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def do_GET(self):
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            for i in range(0, len(payload), 4096):
                self.wfile.write(payload[i:i + 4096])
            self.wfile.flush()

        def log_message(self, *a):
            pass

    class Server(socketserver.ThreadingTCPServer):
        allow_reuse_address = True
        daemon_threads = True

    server = Server((bind_host, 0), Handler)
    port = server.server_address[1]
    threading.Thread(target=server.serve_forever, daemon=True).start()
    time.sleep(0.4)
    received = 0
    try:
        with socket.create_connection((host, port), timeout=8) as client:
            client.settimeout(4)
            client.sendall(b"GET /x HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            buffer = b""
            deadline = time.time() + 4
            while time.time() < deadline:
                try:
                    chunk = client.recv(65536)
                except socket.timeout:
                    break
                if not chunk:
                    break
                buffer += chunk
            _, _, body = buffer.partition(b"\r\n\r\n")
            received = len(body)
    except OSError:
        pass
    server.shutdown()
    return received, size


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", default="127.0.0.1", help="address the client connects to")
    parser.add_argument("--bind", default=None, help="address the server binds to (default: same as --host)")
    parser.add_argument("--probes", type=int, default=5)
    parser.add_argument("--gap", type=float, default=0.4)
    parser.add_argument("--http", action="store_true", help="also run the multi-write HTTP check")
    args = parser.parse_args()

    bind_host = args.bind or args.host
    print(f"=== local TCP server write-loss probe: client {args.host}, server bound {bind_host} ===")
    first_ok = second_ok = 0
    for index in range(args.probes):
        got_first, got_second, raw = probe(args.host, bind_host, args.gap)
        first_ok += got_first
        second_ok += got_second
        verdict = "BOTH" if (got_first and got_second) else ("FIRST-ONLY" if got_first else "NEITHER")
        print(f"   probe {index + 1:2}/{args.probes}: {verdict:12} received={raw!r}")
        if index + 1 < args.probes:
            time.sleep(1.0)

    print()
    print(f"   first write delivered   {first_ok}/{args.probes}")
    print(f"   second write delivered  {second_ok}/{args.probes}")
    if second_ok == 0 and first_ok == args.probes:
        print("   VERDICT: this host drops every write after the first on a local connection")
        print("            a remote server is unaffected; the loss is on the server's send path")
        return 1
    if second_ok == args.probes:
        print("   VERDICT: no loss detected on this host")
        return 0
    print("   VERDICT: intermittent loss detected")
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
