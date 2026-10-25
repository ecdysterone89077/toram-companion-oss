# Toram Companion

[![CI](https://github.com/ecdysterone89077/toram-companion-oss/actions/workflows/ci.yml/badge.svg)](https://github.com/ecdysterone89077/toram-companion-oss/actions/workflows/ci.yml)
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
- **Critical-damage ladder** — selectable +100/+200/+300 tiers; the
  [historical observations](specs/SAFE_CRIT_LADDER_20261019.md) do not establish
  universal acceptance limits.
  The historical +1000 override remains available for compatibility but is
  labelled UNSAFE; its failure mode is documented in
  [critical-damage record](specs/CRIT_DAMAGE_1000_20261019.md).
- **Critical-rate and MP ladders** — selectable ladders with the same
  ownership and restore rules.
- **Capture channel** — timestamped local/received combat observations written
  to a per-process capture file for offline analysis.

## Build

Prerequisites:

- Windows x64
- Rust 1.99 or newer (`rustup default stable`)
- Visual Studio 2022 C++ Build Tools with the Windows SDK
- PowerShell 7 for the release packager
- Python 3.11 or newer for the offline script checks

From the repository root, test, build, and stage the four release artifacts:

```powershell
cargo xtask build
```

This stages `companion.exe` and `observer.exe` in the repository root, with
`wcore.dll` and `wboot.exe` under `src/`. For a compile-only build, use
`cargo build --release --workspace --locked --offline`; its outputs stay in
`target/x86_64-pc-windows-msvc/release/`.

After staging, create Windows and source archives with checksums and a manifest.
Read the version from the workspace so the package label matches the source:

```powershell
$version = python -c "import tomllib; print(tomllib.load(open('Cargo.toml', 'rb'))['workspace']['package']['version'])"
pwsh -NoProfile -File ./package-release.ps1 -Version $version
```

The packager verifies archive entries against the staged inputs and writes
outputs under `dist/`. It packages existing binaries; it does not build them
or establish reproducible source-to-binary correspondence.

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
capture format are documented in the [design-record index](specs/README.md).

## Testing

```powershell
python -B -m unittest discover -s scripts -p '*_checks.py'
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
cargo test --workspace --locked --offline
pwsh -NoProfile -File scripts/package_release_checks.ps1
```

The workspace suite includes a separate-process reader test and the movement
field restore checks. CI runs these gates on pushes to `main`, pull requests,
and manual dispatch. Python checks that require private metadata or incident archives
are skipped when those inputs are absent. Packaging checks use synthetic files.
See the [script index](scripts/README.md) for each tool's inputs and behavior.

## Documentation

- [`CHANGELOG.md`](CHANGELOG.md) — release history
- [`specs/README.md`](specs/README.md) — design records and bug registry by topic
- [`scripts/README.md`](scripts/README.md) — offline checks and research utilities
- [`CONTRIBUTING.md`](CONTRIBUTING.md) — workflow and review conventions
- [`SECURITY.md`](SECURITY.md) — vulnerability reporting and intended-use scope

## License

MIT — see [`LICENSE`](LICENSE).
