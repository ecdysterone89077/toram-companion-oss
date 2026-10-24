# Toram Companion

[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.99%2B-orange.svg)](https://www.rust-lang.org)
[![Platform](https://img.shields.io/badge/platform-Windows%20x64-lightgrey.svg)](#build)

An open-source runtime instrumentation and research framework for the Unity
IL2CPP client of *Toram Online* (Asobimo, Inc.). The framework attaches to a
running client process, resolves gameplay functions from the live IL2CPP
metadata at runtime, and exposes a controlled set of memory features through a
local GUI/CLI companion, an in-process core, and a separate telemetry channel.

Built as a pure-Rust workspace with **zero external crate dependencies** —
every crate depends only on other crates in this repository, so the whole
build is auditable from source.

> **Disclaimer.** This project is not affiliated with, endorsed by, or
> connected to Asobimo, Inc. or *Toram Online*. All game names and trademarks
> belong to their respective owners and are used here for identification only.
> The repository contains no game assets, no game dumps, and no copyrighted
> game content — only original source code and original documentation.
>
> The software is provided for interoperability research and education, on
> builds you own or are explicitly authorized to test. Running it against a
> live game service may violate that service's terms of use; you are solely
> responsible for your own accounts and for complying with applicable terms.
> See [SECURITY.md](SECURITY.md) for the intended-use scope.

## Architecture

| Crate | Role |
| --- | --- |
| `crates/protocol` | Shared-memory IPC telemetry frame between the in-process core and the observer |
| `crates/wcore` | In-process core: IL2CPP runtime resolution, hook installation, feature implementations, capture channel |
| `crates/wboot` | Loader: stages a content-hashed shadow copy of the core and loads it into the target process |
| `crates/companion` | Win32 companion: GUI and CLI command surface, process discovery, lifecycle management |
| `crates/observer` | Separate-process telemetry reader: reads the capture stream and never touches the game |
| `crates/xtask` | Build and check helper tasks |

Every feature owns its writes: originals are captured at activation, a failed
activation rolls back, and OFF restores the captured values exactly.

## Features

- **Movement speed** — five native speed getters and the dash getter return a
  scaled envelope instead of the native value; OFF restores the captured
  originals byte-for-byte.
- **No collision** — gravity, ignore-height, move-height-range, floor-hit and
  on-ground-move fields, each with its own original-value capture and rollback.
- **Critical-damage ladder** — the server-accepted tiers are +100/+200/+300.
  The historical +1000 override remains available for compatibility but is
  labelled UNSAFE; its failure mode is documented in
  `specs/CRIT_DAMAGE_1000_20261019.md`.
- **Critical-rate and MP ladders** — accepted-value ladders with the same
  ownership and restore rules.
- **Capture channel** — timestamped local/received combat observations written
  to a per-process capture file for offline analysis.

## Build

Prerequisites:

- Windows x64
- Rust 1.99 or newer (`rustup default stable`)
- PowerShell 7 for the release packager

```powershell
cargo build --release --workspace
```

A reproducible release package (Windows bundle plus source archive, every
archive entry read back and verified, with checksums and a manifest) is
produced by:

```powershell
pwsh ./package-release.ps1 -Version 0.96.4
```

## Usage

Start the companion against a running client process:

```powershell
.\companion.exe                       # GUI
.\companion.exe --clients             # list attachable processes
.\companion.exe --cmd "1" --pid 1234  # run one command band against a PID
```

Read telemetry in a separate process (the observer never touches the game):

```powershell
.\observer.exe --pid 1234 --seconds 60
```

Command bands are numbered; `0` is the unload/everything-off band. The
lifecycle rules — activation rollback, OFF restore, worker recovery — and the
capture format are documented in `specs/`.

## Testing

```powershell
cargo fmt   --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test  --workspace --locked
```

The workspace suite includes a separate-process reader test and the movement
field restore checks. Continuous integration runs the same gates on every
push. Additional offline audit tools live in `scripts/`; they use only the
Python standard library and never open a process.

## Documentation

- [`CHANGELOG.md`](CHANGELOG.md) — release history
- `specs/` — design records: movement safety rework, the validated dimension
  model behind server acceptance, critical-damage acceptance, MP analysis,
  stability analysis, and the bug registry
- [`CONTRIBUTING.md`](CONTRIBUTING.md) — workflow and review conventions
- [`SECURITY.md`](SECURITY.md) — vulnerability reporting and intended-use scope

## License

MIT — see [`LICENSE`](LICENSE).
