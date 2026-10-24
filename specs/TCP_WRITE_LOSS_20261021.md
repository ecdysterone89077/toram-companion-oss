# The local TCP write loss on this host — 2026-10-21

**Status:** confirmed, measured, and reproducible. This is the mechanism behind
REA's Ghidra bridge hanging after its first reply. It is also a live hazard
for the project's open attribution question, and it is **not** a defect in the
companion.

**Related:** the REA repair record, the REA project profile and the incident
attribution (development repository); [the validated dimension
model](VALIDATED_DIMENSION_MODEL_20261021.md).

---

## 1. The finding

**A TCP connection accepted by a server on this host delivers only the bytes
of its first `write()`. Every later `write()` on that connection is silently
discarded.** The peer receives the first frame and then nothing: no error,
no reset, no close. The loss is on the **send side**, so it is the server
that is broken and the client that is truncated.

Reproduce it:

```powershell
python scripts\tcp_write_probe.py
```

```
   probe  1/3: FIRST-ONLY   received=b'FIRST\n'
   probe  2/3: FIRST-ONLY   received=b'FIRST\n'
   probe  3/3: FIRST-ONLY   received=b'FIRST\n'

   first write delivered   3/3
   second write delivered  0/3
   VERDICT: this host drops every write after the first on a local connection
```

## 2. What was measured

| trial | result |
| --- | --- |
| two writes, no delay | first only |
| two writes, 400 ms apart | first only |
| three writes | first only |
| **one write carrying both frames** | **both arrive** |
| server socket with `TCP_NODELAY` | unchanged |
| peer keeps sending between the two writes | unchanged |
| 12 consecutive probes over 40 s | 12/12 first, **0/12** second |
| loopback `127.0.0.1` | loss |
| LAN `192.168.100.16` | loss |
| virtual adapters `172.20.240.1`, `172.27.208.1` | loss |
| client in a separate process | loss |
| client implemented in Python, Java, Node, Rust | loss in all four |
| **remote** server, 201 KB HTTPS response via `curl.exe` | **complete** |

The behaviour is deterministic, not intermittent: 12/12 and 0/12. It is not a
timing window and not a per-language artifact. The one thing that changes
the outcome is **how many `write()` calls the server makes**, not what it
writes, how big it is, or how fast.

A local HTTP server writing a 200 KB body in 49 chunks delivers **0 body
bytes** to `curl.exe`, Node and Java clients alike — the headers arrive, the
body does not. A remote server's 201 KB response arrives complete through
the same client stack.

## 3. What it explains, and what it does not

**It explains REA's Ghidra bridge.** REA 6.7.0's bridge serves one
newline-framed JSON reply per request on one persistent socket. The startup
handshake `ping` is the first write, so it arrives; every real query after
it is the second write, so it never does. The symptom looks like a broken
bridge and is actually a broken local socket. Instrumenting the bridge
confirmed the reply was produced and `flush()` returned normally before the
bytes vanished.

**It does not affect the companion.** The project's crates use named pipes
only — `TcpListener`, `TcpStream` and `UdpSocket` do not appear anywhere in
`crates/`. The companion's IPC is unaffected, which is consistent with every
observed session.

**It does not, by itself, explain the game's incidents.** The game is a
client to a remote server, and remote servers are unaffected. But it
changes what the next investigation has to ask. If anything on this machine
sits in the game's network path — a local proxy, a traffic filter, a
tunnelled route — then it is a local sender to the game, and it would be
subject to exactly this loss. That is a live candidate for
"Memory didn't respond correctly" and for the mid-session "Server
Disconnected", and it is a candidate that needs none of our code to be
present. It fits the 2026-10-21 03:06 incident exactly, where the core was
never loaded.

## 4. Candidates on this host

Three security products are installed, all with auto-start and network
filtering capability:

| component | state | note |
| --- | --- | --- |
| Reason Labs EPP — `rsEngineSvc` | running, auto | endpoint protection engine with network filtering |
| Reason Labs "Safer Web" DNS — `rsDNSResolver`, `rsDNSSvc` | running, auto | DNS interception, and a filter alongside it |
| Reason Labs RAV VPN — `rsVPNSvc` | running, auto | VPN service, install filter capable |
| Proton VPN | service stopped, manual | WireGuard service present, install 2026-10-12 |
| Web Threat Defender — `webthreatdefusersvc` | **running, auto** | third-party filter product |

Also present: LDPlayer 4 (`C:\LDPlayer\LDPlayer14`, install folder touched
2026-10-13, `Ld9BoxSup` driver disabled), an Android emulator whose own
network stack is a known source of host-level interference and which is
itself the kind of thing a game reports as an app that may affect gameplay.

Windows' own `netsh wfp show filters` dump contains sixteen `LDPlayer`
references, so the emulator is registered with the network filtering stack.

None of these has been shown to be the cause. The list is the set of things
worth eliminating, in the order that costs least.

## 5. The test that settles it

**One measurement, in this order, with the game running:**

1. Baseline: `python scripts\tcp_write_probe.py --probes 10` while nothing
   else changes. Expect 0/10 second writes.
2. Stop the three auto-start filter services — `rsEngineSvc`, `rsDNSResolver`
   / `rsDNSSvc`, `rsVPNSvc`, and `webthreatdefusersvc` — and re-run the
   probe.
3. If the loss disappears with one of them stopped, that component is the
   cause. Restart the others and re-run to confirm the loss comes back with
   that one stopped.
4. Re-test with the **game running** and the same measurement. If the loss
   persists with the game up, the game is not on the affected path; if the
   game's own connections start showing truncation, it is.

Step 2 changes the security posture of the machine, so it needs an explicit
go-ahead. The services are set to auto-start, so a reboot restores the
machine to its current state either way.

## 6. Consequences for the project's open questions

**The attribution workstream has been anchored to the wrong incident.** The
2026-10-21 03:06 "Unauthorized Action Detected" happened in a process where
the core was never loaded, which rules out our hook mechanism. A local
network component sitting in the game's path is a candidate that requires
none of our code to be present, and it explains both halves of the banner:
the client stops responding to the server, and the machine has third-party
traffic filters that a scan for "apps that may affect gameplay" would
plausibly surface.

**This does not weaken the validated dimension model.** That model is about
what the server recomputes, and nothing here touches it. The two axes stay
separate: values inside the server's model, and the mechanism that delivers
them.

**It does add a third axis: the path.** Before, the assumption was that the
only two surfaces were the server's value model and the client's memory
integrity. There is a third: whether the client's traffic to the server is
intact. A client that cannot deliver a heartbeat is a client that "did not
respond correctly", whatever it has or has not modified.

**The REA Ghidra lane stays blocked on this host.** The bridge is upstream
and correct; the socket underneath it is not. The metadata, PE, Android and
artifact providers are unaffected, because none of them serves over a local
TCP socket. The native lane becomes usable the moment the local loss is
resolved — nothing in REA needs to change.

## 7. Scope

No game process was launched, no command was dispatched to the companion, no
account was touched, and no security product was stopped or reconfigured.
Every measurement used a loopback or LAN socket between two processes
started for the purpose.

`scripts/tcp_write_probe.py` is the reusable form of the measurement and
exits non-zero when the loss is present, so it can be used as a gate.
