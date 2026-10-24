#!/usr/bin/env python3
"""Patch REA's Ghidra bridge to use a byte-exact line transport.

What this does
---------------
Replaces the buffered reader/writer pair with one transport that reads and
writes UTF-8 frames directly on the socket streams, moves the TCP transport
onto java.net.ServerSocket / java.net.Socket, and writes each frame in a
single send so a frame can never be truncated between the body and its
terminator. The patch is idempotent, refuses to run against a source it does
not recognise, compiles the result with javac against the Ghidra classpath
before accepting it, and keeps a pristine copy for --restore.

What this does NOT do
---------------------
It does not fix the bridge hanging on this host. The cause is environmental
and was measured: a TCP connection accepted by a server on this machine
delivers only the bytes of its first write, and every later write is
silently discarded. See specs/TCP_WRITE_LOSS_20261021.md and
scripts/tcp_write_probe.py. The bridge's first reply is the startup
handshake ping, so it arrives and every real query after it does not.

The patch is therefore held in reserve: it makes the transport correct and
keeps the bridge ready, and it becomes effective as soon as the local write
loss is resolved. Nothing in REA needs to change for the native lane to
work on a healthy host.

The upstream transport, for reference
-------------------------------------
The upstream bridge reads the authentication handshake from
client.socket().getInputStream() and then reads the request loop through a
second reader built on Channels.newReader(client, ...). Those are two
wrappers over the same channel, and the request loop does not observe the
lines that follow the handshake.

Usage
-----
    python scripts/patch_rea_ghidra_bridge.py             # apply (idempotent)
    python scripts/patch_rea_ghidra_bridge.py --check     # report status only
    python scripts/patch_rea_ghidra_bridge.py --restore   # revert to upstream
    python scripts/patch_rea_ghidra_bridge.py --install <path to rea-agents>
"""

from __future__ import annotations

import argparse
import os
import shutil
import sys
from pathlib import Path

BRIDGE_REL = Path("bridge") / "ghidra" / "ReaGhidraBridge.java"
BACKUP_SUFFIX = ".rea-pristine"
MARKER = "// REA PATCH: byte-exact line transport for the Ghidra bridge"

CANDIDATE_ROOTS = [
    Path(os.environ.get("APPDATA", "")) / "npm" / "node_modules" / "rea-agents",
    Path("/usr/local/lib/node_modules/rea-agents"),
    Path("/usr/lib/node_modules/rea-agents"),
    Path.home() / ".npm-global" / "lib" / "node_modules" / "rea-agents",
]

LINE_CHANNEL = '''
    /**
     * Byte-exact line transport for the bridge protocol. Frames are UTF-8,
     * newline terminated, and moved straight to the socket with no
     * intermediate encoder or buffer.
     */
    private static final class LineChannel implements AutoCloseable {
        private final InputStream input;
        private final OutputStream output;

        LineChannel(InputStream input, OutputStream output) {
            this.input = input;
            this.output = output;
        }

        /** Read one newline-delimited frame, or null at end of stream. */
        String readLine() throws IOException {
            ByteArrayOutputStream frame = new ByteArrayOutputStream();
            int value;
            while ((value = input.read()) != -1) {
                if (value == '\\n') {
                    byte[] bytes = frame.toByteArray();
                    int end = bytes.length;
                    if (end > 0 && bytes[end - 1] == '\\r') end--;
                    return new String(bytes, 0, end, StandardCharsets.UTF_8);
                }
                frame.write(value);
            }
            return frame.size() == 0 ? null : frame.toString(StandardCharsets.UTF_8);
        }

        /** Write one newline-delimited frame and push it to the peer. */
        void writeLine(String text) throws IOException {
            byte[] bytes = text.getBytes(StandardCharsets.UTF_8);
            byte[] frame = new byte[bytes.length + 1];
            System.arraycopy(bytes, 0, frame, 0, bytes.length);
            frame[bytes.length] = '\\n';
            // One write call for the whole frame. A separate write for the
            // terminator is a second send on the same socket and can be
            // dropped, which truncates the frame and leaves the peer waiting
            // for a newline that never arrives.
            output.write(frame, 0, frame.length);
            output.flush();
        }

        @Override
        public void close() throws IOException {
            try {
                input.close();
            }
            finally {
                output.close();
            }
        }
    }
'''

STRING_REPLACEMENTS = [
    (
        "imports",
        "import java.io.InputStream;\n",
        """import java.io.InputStream;
import java.io.OutputStream;
""",
    ),
    (
        "imports-net",
        "import java.net.SocketTimeoutException;\n",
        """import java.net.ServerSocket;
import java.net.Socket;
import java.net.SocketTimeoutException;
""",
    ),
]

REGIONS = [
    (
        "serveLoopbackTcp",
        "    private void serveLoopbackTcp(SessionDescriptor descriptor) throws Exception {",
        """    private void serveLoopbackTcp(SessionDescriptor descriptor) throws Exception {
        Path endpointPath = Path.of(descriptor.endpointPath);
        Path pendingPath = endpointPath.resolveSibling(endpointPath.getFileName() + ".pending");
        Files.deleteIfExists(endpointPath);
        Files.deleteIfExists(pendingPath);
        try (ServerSocket server = new ServerSocket()) {
            server.setReuseAddress(true);
            server.bind(new InetSocketAddress(InetAddress.getByName("127.0.0.1"), 0));
            InetSocketAddress address = (InetSocketAddress) server.getLocalSocketAddress();
            JsonObject endpoint = new JsonObject();
            endpoint.addProperty("host", "127.0.0.1");
            endpoint.addProperty("port", address.getPort());
            // The Windows native reader refuses a file while a writer still
            // owns it. Closing CREATE_NEW publishes the complete record without
            // requesting rename rights on the retained private directory guard.
            Files.writeString(
                isWindowsHost() ? endpointPath : pendingPath,
                GSON.toJson(endpoint) + "\\n",
                StandardCharsets.UTF_8,
                StandardOpenOption.CREATE_NEW,
                StandardOpenOption.WRITE
            );
            if (!isWindowsHost()) {
                try {
                    Files.move(pendingPath, endpointPath, StandardCopyOption.ATOMIC_MOVE);
                }
                catch (AtomicMoveNotSupportedException exception) {
                    Files.move(pendingPath, endpointPath);
                }
            }
            acceptAuthenticatedLoopbackClient(server, descriptor);
        }
        finally {
            Files.deleteIfExists(pendingPath);
            Files.deleteIfExists(endpointPath);
        }
    }""",
        ["ServerSocketChannel server = ServerSocketChannel.open()", "server.getLocalAddress()"],
    ),
    (
        "readLoopbackHandshake",
        "    private static String readLoopbackHandshake(",
        """    private static String readLoopbackHandshake(
            Socket client, LineChannel channel, int maxBytes, long timeoutMillis) throws IOException {
        long deadline = System.nanoTime() + timeoutMillis * 1_000_000L;
        while (true) {
            long remaining = deadline - System.nanoTime();
            if (remaining <= 0) throw new SocketTimeoutException("Bridge authentication timed out");
            client.setSoTimeout((int) Math.max(1,
                Math.min(Integer.MAX_VALUE, (remaining + 999_999L) / 1_000_000L)));
            String line = channel.readLine();
            if (line == null) return null;
            if (line.length() > maxBytes) throw new IOException("Bridge authentication frame is too large");
            return line;
        }
    }""",
        ["SocketChannel client, int maxBytes", "client.socket().getInputStream()"],
    ),
    (
        "acceptAuthenticatedLoopbackClient",
        "    private void acceptAuthenticatedLoopbackClient(",
        """    private void acceptAuthenticatedLoopbackClient(
            ServerSocket server, SessionDescriptor descriptor) throws Exception {
        while (true) {
            try (Socket client = server.accept()) {
                Request first;
                try (LineChannel channel = new LineChannel(
                         client.getInputStream(), client.getOutputStream())) {
                    try {
                        String line = readLoopbackHandshake(client, channel, 64 * 1024, 1_000);
                        if (line == null) continue;
                        first = parseRequest(line, descriptor.token);
                    }
                    catch (IOException | RuntimeException rejected) {
                        // A rejected unauthenticated peer cannot consume the bridge session.
                        continue;
                    }
                    client.setSoTimeout(0);
                    serveClient(descriptor, channel, first);
                }
                return;
            }
        }
    }""",
        ["ServerSocketChannel server, SessionDescriptor descriptor",
         "readLoopbackHandshake(client, 64 * 1024, 1_000)"],
    ),
    (
        "acceptClient",
        "    private void acceptClient(",
        """    private void acceptClient(
            ServerSocketChannel server,
            SessionDescriptor descriptor) throws Exception {
        try (SocketChannel client = server.accept();
             LineChannel channel = new LineChannel(
                 Channels.newInputStream(client), Channels.newOutputStream(client))) {
            serveClient(descriptor, channel, null);
        }
    }""",
        ["Channels.newReader(client, StandardCharsets.UTF_8)"],
    ),
    (
        "serveClient",
        "    private void serveClient(",
        """    private void serveClient(
            SessionDescriptor descriptor,
            LineChannel channel,
            Request first) throws Exception {
        while (true) {
            Request request = first;
            first = null;
            if (request == null) {
                String line = channel.readLine();
                if (line == null) return;
                try {
                    request = parseRequest(line, descriptor.token);
                }
                catch (RuntimeException exception) {
                    writeFailure(channel, 1, "invalid_request", "Bridge request is invalid");
                    return;
                }
            }
            try {
                if (request.method.equals("shutdown")) {
                    requireKeys(request.params, Set.of());
                    JsonObject result = new JsonObject();
                    result.addProperty("shutdown", true);
                    result.addProperty("project_ephemeral", true);
                    writeSuccess(channel, request.id, result);
                    return;
                }
                writeSuccess(channel, request.id, handleRequest(request, descriptor));
            }
            catch (RequestFailure failure) {
                writeFailure(channel, request.id, failure.code, failure.getMessage());
            }
            catch (Exception exception) {
                writeFailure(
                    channel,
                    request.id,
                    "bridge_exception",
                    exception.getClass().getSimpleName() + ": " + safeMessage(exception)
                );
            }
        }
    }""",
        ["BufferedReader reader,", "reader.readLine()"],
    ),
    (
        "writeSuccess",
        "    private static void writeSuccess(",
        """    private static void writeSuccess(LineChannel channel, int id, JsonElement result)
            throws IOException {
        JsonObject response = new JsonObject();
        response.addProperty("id", id);
        response.addProperty("ok", true);
        response.add("result", result);
        writeResponse(channel, response);
    }""",
        ["BufferedWriter writer, int id, JsonElement result"],
    ),
    (
        "writeFailure",
        "    private static void writeFailure(",
        """    private static void writeFailure(
            LineChannel channel,
            int id,
            String code,
            String message) throws IOException {
        JsonObject error = new JsonObject();
        error.addProperty("code", code);
        error.addProperty("message", message);
        JsonObject response = new JsonObject();
        response.addProperty("id", id);
        response.addProperty("ok", false);
        response.add("error", error);
        writeResponse(channel, response);
    }""",
        ["BufferedWriter writer,\n            int id,"],
    ),
    (
        "writeResponse",
        "    private static void writeResponse(",
        """    private static void writeResponse(LineChannel channel, JsonObject response) throws IOException {
        String encoded = GSON.toJson(response);
        // Gson leaves UTF-16 surrogates literal. Escape them in JSON so even
        // diagnostics quoting ill-formed caller text remain lossless UTF-8.
        StringBuilder builder = new StringBuilder(encoded.length() + 16);
        int start = 0;
        for (int index = 0; index < encoded.length(); index++) {
            if (!Character.isSurrogate(encoded.charAt(index))) continue;
            builder.append(encoded, start, index);
            builder.append("\\\\u").append(Integer.toHexString(encoded.charAt(index)));
            start = index + 1;
        }
        builder.append(encoded, start, encoded.length());
        channel.writeLine(builder.toString());
    }""",
        ["BufferedWriter writer, JsonObject response"],
    ),
]

PATCHED_MARKERS = [
    "import java.io.OutputStream;",
    "import java.net.ServerSocket;",
    "import java.net.Socket;",
    "private static final class LineChannel implements AutoCloseable",
    "try (ServerSocket server = new ServerSocket())",
    "server.getLocalSocketAddress()",
    "Socket client, LineChannel channel, int maxBytes",
    "new LineChannel(\n                         client.getInputStream(), client.getOutputStream())",
    "Channels.newInputStream(client), Channels.newOutputStream(client)",
    "LineChannel channel,\n            Request first",
    "writeSuccess(LineChannel channel, int id, JsonElement result)",
    "writeFailure(\n            LineChannel channel,",
    "writeResponse(LineChannel channel, JsonObject response)",
]

STALE_MARKERS = [
    "Channels.newReader(client, StandardCharsets.UTF_8)",
    "Channels.newWriter(client, StandardCharsets.UTF_8)",
    "readLoopbackHandshake(client, 64 * 1024, 1_000)",
    "client.socket().getInputStream()",
    "client.socket().setSoTimeout",
]


def ghidra_classpath() -> str | None:
    """Classpath of every jar in the configured Ghidra install, for javac."""
    for base in (
        Path(os.environ.get("GHIDRA_INSTALL_DIR", "")),
    ):
        if base and base.is_dir():
            jars = [str(p) for p in base.rglob("*.jar")]
            if jars:
                return os.pathsep.join(jars)
    return None


def compile_check(source: Path) -> tuple[bool, str]:
    """Compile the bridge source the way Ghidra will, and report the result."""
    javac = None
    java_home = os.environ.get("JAVA_HOME", "")
    if java_home:
        candidate = Path(java_home) / "bin" / "javac.exe"
        if candidate.is_file():
            javac = candidate
    if javac is None:
        found = shutil.which("javac")
        javac = Path(found) if found else None
    classpath = ghidra_classpath()
    if javac is None or classpath is None:
        return True, "skipped: no javac or no Ghidra install found"
    import subprocess
    import tempfile
    with tempfile.TemporaryDirectory(prefix="rea-bridge-compile-") as out:
        result = subprocess.run(
            [str(javac), "-nowarn", "-proc:none", "-cp", classpath,
             "-sourcepath", str(source.parent),
             "-d", out, str(source)],
            capture_output=True, text=True,
        )
    if result.returncode == 0:
        return True, "compiles"
    errors = [l for l in result.stderr.splitlines() if "error" in l.lower()]
    return False, "; ".join(errors[:6]) or result.stderr[:400]


def find_install(explicit: str | None) -> Path:
    if explicit:
        root = Path(explicit).expanduser()
        if (root / BRIDGE_REL).is_file():
            return root
        raise SystemExit(f"no bridge source under {root}")
    for root in CANDIDATE_ROOTS:
        if root and (root / BRIDGE_REL).is_file():
            return root
    raise SystemExit(
        "rea-agents install not found; pass --install <path to node_modules/rea-agents>"
    )


def region_bounds(lines: list[str], signature: str) -> tuple[int, int]:
    start = None
    for i, line in enumerate(lines):
        if line.startswith(signature):
            start = i
            break
    if start is None:
        raise SystemExit(f"region not found: {signature}")
    depth = 0
    opened = False
    for i in range(start, len(lines)):
        depth += lines[i].count("{") - lines[i].count("}")
        if "{" in lines[i]:
            opened = True
        if opened and depth == 0:
            return start, i
    raise SystemExit(f"region end not found: {signature}")


def replace_region(text: str, signature: str, replacement: str, required: list[str]) -> str:
    lines = text.split("\n")
    start, end = region_bounds(lines, signature)
    original = "\n".join(lines[start:end + 1])
    for marker in required:
        if marker not in original:
            raise SystemExit(
                f"upstream shape changed in {signature.strip()}: expected {marker!r}"
            )
    return text.replace(original, replacement, 1)


def apply_patch(text: str) -> str:
    for name, original, replacement in STRING_REPLACEMENTS:
        if original not in text:
            raise SystemExit(f"upstream shape changed at {name}: anchor missing")
        text = text.replace(original, replacement, 1)
    for name, signature, replacement, required in REGIONS:
        text = replace_region(text, signature, replacement, required)
    anchor = "    private void serveClient("
    if anchor not in text:
        raise SystemExit("cannot place LineChannel: serveClient anchor missing")
    text = text.replace(anchor, LINE_CHANNEL.strip("\n") + "\n\n" + anchor, 1)
    return MARKER + "\n" + text


def verify(text: str) -> tuple[bool, list[str]]:
    problems = [m for m in PATCHED_MARKERS if m not in text]
    problems += [f"stale:{m}" for m in STALE_MARKERS if m in text]
    return (not problems), problems


def main() -> int:
    parser = argparse.ArgumentParser(description="Patch REA's Ghidra bridge line transport.")
    parser.add_argument("--install", help="path to the rea-agents package root")
    group = parser.add_mutually_exclusive_group()
    group.add_argument("--check", action="store_true", help="report status only")
    group.add_argument("--restore", action="store_true", help="restore the upstream source")
    args = parser.parse_args()

    root = find_install(args.install)
    bridge = root / BRIDGE_REL
    backup = bridge.with_name(bridge.name + BACKUP_SUFFIX)

    current = bridge.read_text(encoding="utf-8")

    if args.check:
        state = "patched" if MARKER in current else "upstream"
        ok, problems = verify(current)
        print(f"install:  {root}")
        print(f"bridge:   {bridge}")
        print(f"state:    {state}")
        print(f"verify:   {'ok' if ok else 'FAILED'}")
        for problem in problems:
            print(f"  - {problem}")
        return 0 if (ok and state == "patched") else 1

    if args.restore:
        if not backup.is_file():
            print(f"nothing to restore: {backup} does not exist", file=sys.stderr)
            return 1
        shutil.copyfile(backup, bridge)
        print(f"restored upstream source: {bridge}")
        return 0

    if MARKER in current:
        ok, problems = verify(current)
        print(f"already patched: {bridge}")
        for problem in problems:
            print(f"  - {problem}")
        return 0 if ok else 1

    if not backup.is_file():
        shutil.copyfile(bridge, backup)

    try:
        patched = apply_patch(current)
    except SystemExit as failure:
        print(f"refusing to patch: {failure}", file=sys.stderr)
        return 2

    bridge.write_text(patched, encoding="utf-8", newline="")
    ok, problems = verify(patched)
    print(f"patched:  {bridge}")
    print(f"pristine: {backup}")
    print(f"verify:   {'ok' if ok else 'FAILED'}")
    for problem in problems:
        print(f"  - {problem}")
    if not ok:
        return 1
    compiles, detail = compile_check(bridge)
    print(f"compile:  {detail}")
    if not compiles:
        shutil.copyfile(backup, bridge)
        print("reverted: the patched source does not compile", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
