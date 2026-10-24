# Contributing to Toram Companion Framework

Thank you for your interest in contributing to the **Toram Companion Framework**! We welcome contributions that help improve code quality, documentation, test coverage, and systems research.

As an open-source project focused on reverse engineering research, systems programming, and high-performance Rust on Windows, we adhere to high standards of technical excellence, transparency, and ethical conduct.

---

## Code of Conduct

This project and everyone participating in it is governed by our [Code of Conduct](CODE_OF_CONDUCT.md). By participating, you are expected to uphold this code. Please report unacceptable behavior following the reporting instructions in that file.

---

## How Can I Contribute?

### 1. Reporting Bugs
- Ensure the bug was not already reported by searching on GitHub under [Issues](https://github.com/ecdysterone89077/toram-companion-oss/issues).
- If you're unable to find an open issue addressing the problem, open a new one using our **Bug Report** template.
- Include detailed system information (Windows version, Rust toolchain version, game client patch level) and exact reproduction steps.

### 2. Suggesting Enhancements
- Open an issue using our **Feature Request** template.
- Provide a clear and concise description of the suggested enhancement and its technical motivation.

### 3. Pull Requests
1. **Fork the repo** and create your branch from `main`:
   ```bash
   git checkout -b feature/my-new-feature
   ```
2. **Follow Rust Idioms & Formatting**:
   - Ensure the code compiles cleanly on stable Rust (`x86_64-pc-windows-msvc`).
   - Run `cargo fmt` and ensure no compilation warnings:
     ```bash
     cargo check --workspace --all-targets
     ```
3. **Write & Run Tests**:
   - Add unit tests for any new functionality, offset resolution, or protocol messages.
   - Run the automated test suite before opening your PR:
     ```powershell
     cargo xtask test
     ```
4. **Adhere to Open Source & Legal Guidelines**:
   - Do **NOT** commit copyrighted game assets, decompiled proprietary source code dumps (e.g. `dump.cs`), or proprietary binaries (`.exe`, `.dll`, `.apk`).
   - Do **NOT** commit any personal access tokens, passwords, or secrets.
   - Keep all code under the [MIT License](LICENSE).

---

## Development Environment Setup

### Prerequisites
- 64-bit Windows 10/11
- [Rust](https://www.rust-lang.org/tools/install) (stable `x86_64-pc-windows-msvc`)
- Visual Studio 2022 C++ Build Tools (with Windows SDK)
- PowerShell 7 for release packaging

### Workspace Build Commands
- Build all debug targets:
  ```bash
  cargo build --workspace
  ```
- Build optimized release binaries:
  ```powershell
  cargo xtask build
  ```
- Run unit test suite:
  ```powershell
  cargo xtask test
  ```

---

## Commit Message Guidelines

We follow [Conventional Commits](https://www.conventionalcommits.org/):
- `feat(scope): add new telemetry counter`
- `fix(scope): resolve vtable slot calculation edge case`
- `docs(scope): update README architecture diagram`
- `test(scope): add test for method signature resolution`
- `refactor(scope): streamline shadow DLL dispatcher`

Thank you for contributing to open-source systems research!
