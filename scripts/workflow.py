"""Compact project context and change-scoped checks. Python 3.11+, stdlib only."""

from __future__ import annotations

import argparse
from collections import deque
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time
import tomllib
import unittest
import uuid

ROOT = Path(__file__).resolve().parents[1]
DOC_SUFFIXES = {".md", ".rst"}
DOC_NAMES = {"LICENSE", "skills-lock.json"}
SHARED = {"Cargo.toml", "Cargo.lock", "rust-toolchain", "rust-toolchain.toml"}
POLICY_FILES = {"AGENTS.md", "WORKFLOW.md", "CURRENT.md", "opencode.json", "opencode.jsonc"}
GOVERNANCE_SUITES = {"workflow_checks.py", "prefix_cache_checks.py", "agent_contract_checks.py",
                     "workspace_policy_checks.py"}
DEFAULT_TIMEOUT = 900


def output(root: Path, *args: str) -> str:
    result = subprocess.run(args, cwd=root, capture_output=True, timeout=60)
    if result.returncode:
        raise RuntimeError(result.stderr.decode("utf-8", errors="replace").strip()
                           or f"{args[0]} exited {result.returncode}")
    return result.stdout.decode("utf-8", errors="replace")


def git(root: Path, *args: str) -> str:
    return output(root, "git", "--literal-pathspecs", *args)


def records(value: str) -> list[str]:
    return [part for part in value.split("\0") if part]


def changed_paths(root: Path, base: str = "HEAD") -> list[str]:
    if base.startswith("-"):
        raise ValueError("--base must be a Git commit or ref, not an option")
    # No rename detection: both sides of moves must influence check selection.
    tracked = records(git(root, "diff", "--name-only", "--no-renames", "-z", base, "--"))
    untracked = records(git(root, "ls-files", "--others", "--exclude-standard", "-z"))
    return sorted(set(tracked + untracked))


def select_paths(root: Path, requested: list[str], base: str) -> list[str]:
    if not requested:
        return changed_paths(root, base)
    selected: set[str] = set()
    for value in requested:
        candidate = (root / value).resolve()
        try:
            relative = candidate.relative_to(root.resolve()).as_posix()
        except ValueError as exc:
            raise ValueError(f"Path is outside the repository: {value}") from exc
        matches = records(git(root, "ls-files", "--cached", "--others",
                              "--exclude-standard", "-z", "--", relative))
        matches += records(git(root, "diff", "--name-only", "--no-renames",
                               "-z", base, "--", relative))
        if candidate.is_file():
            matches.append(relative)
        if not matches:
            raise ValueError(f"No files match --path {value}")
        selected.update(matches)
    return sorted(selected)


def cargo_graph(root: Path) -> tuple[dict[str, str], dict[str, set[str]]]:
    metadata = json.loads(output(root, "cargo", "metadata", "--no-deps",
                                 "--format-version", "1", "--locked", "--offline"))
    members = set(metadata["workspace_members"])
    packages = [p for p in metadata["packages"] if p["id"] in members]
    directories = {
        Path(p["manifest_path"]).resolve().parent.relative_to(root.resolve()).as_posix(): p["name"]
        for p in packages
    }
    names = set(directories.values())
    dependencies = {
        p["name"]: {d["name"] for d in p["dependencies"] if d["name"] in names}
        for p in packages
    }
    return directories, dependencies


def dependents(seeds: set[str], dependencies: dict[str, set[str]]) -> set[str]:
    selected = set(seeds)
    while True:
        expanded = selected | {name for name, deps in dependencies.items() if deps & selected}
        if expanded == selected:
            return selected
        selected = expanded


def make_plan(root: Path, paths: list[str], base: str = "HEAD") -> tuple[list[list[str]], list[str]]:
    if not paths:
        return [], []
    commands = [[sys.executable, "scripts/workflow.py", "whitespace", "--base", base,
                 *[arg for path in paths for arg in ("--path", path)]]]
    rust_checks: list[list[str]] = []
    manual: list[str] = []
    rust_paths: list[str] = []
    suites: set[str] = set()
    packaging = False
    full = False
    policy = False
    prefix = False
    python_paths: list[str] = []
    for path in paths:
        name = Path(path).name
        policy |= path in POLICY_FILES or path == ".gitattributes" or path.startswith("prompts/") or path in {
            "scripts/workspace_policy.py", "scripts/workspace_policy_checks.py",
            "scripts/prefix_cache.py", "scripts/prefix_cache_checks.py"}
        prefix |= path == "AGENTS.md" or path.startswith("prompts/") or name in {
            "opencode.json", "opencode.jsonc", "prefix_cache.py", "prefix_cache_checks.py"}
        if path in {"scripts/workflow.py", "scripts/workflow_checks.py"}:
            suites.update(GOVERNANCE_SUITES)
            policy = True
        if path.startswith("prompts/") or path in {"scripts/agent_contract.py", "scripts/agent_contract_checks.py"}:
            suites.add("agent_contract_checks.py")
        if path.endswith(".py") and (root / path).is_file():
            python_paths.append(path)
        if Path(path).suffix in DOC_SUFFIXES or name in DOC_NAMES or path in POLICY_FILES or path == ".gitattributes":
            continue
        if path in SHARED or path.startswith(".cargo/"):
            full = True
        elif path.startswith("crates/"):
            rust_paths.append(path)
            full |= name == "Cargo.toml" or path.startswith("crates/protocol/")
        elif path in {"package-release.ps1", "scripts/package_release_checks.ps1"}:
            packaging = True
        elif path.startswith("scripts/") and path.endswith(".py"):
            suite = name if name.endswith("_checks.py") else Path(path).stem + "_checks.py"
            if (root / "scripts" / suite).is_file():
                suites.add(suite)
            else:
                manual.append(path)
            if not (root / path).is_file():
                manual.append(path)
        else:
            manual.append(path)
    if full or rust_paths:
        directories, dependencies = cargo_graph(root)
        seeds: set[str] = set()
        for path in rust_paths:
            owners = [name for directory, name in directories.items()
                      if path.startswith(directory + "/")]
            if owners:
                seeds.update(owners)
            else:
                manual.append(path)
        selected = dependents(seeds, dependencies)
        if full or selected:
            flags = ["--workspace"] if full else [
                arg for name in sorted(selected) for arg in ("-p", name)]
            rust_checks.extend([
                ["cargo", "fmt", *(["--all"] if full else flags), "--", "--check"],
                ["cargo", "test", *flags, "--locked", "--offline"],
                ["cargo", "clippy", *flags, "--all-targets", "--locked", "--offline", "--", "-D", "warnings"],
            ])
    if any(p in {"crates/wcore/src/faunas.rs", "crates/wcore/src/features.rs"} for p in paths):
        suites.add("faunas_target_audit_checks.py")
    if any(p in {"crates/observer/src/main.rs", "crates/wcore/src/combat_response.rs"} for p in paths):
        suites.add("acceptance_report_checks.py")
    if python_paths:
        commands.append([sys.executable, "scripts/workflow.py", "syntax",
                         *[arg for path in sorted(python_paths) for arg in ("--path", path)]])
    if policy:
        suites.add("workspace_policy_checks.py")
        commands.append([sys.executable, "scripts/workspace_policy.py", "check"])
    if prefix:
        suites.add("prefix_cache_checks.py")
        commands.append([sys.executable, "scripts/prefix_cache.py", "check"])
    for suite in sorted(suites):
        commands.append([sys.executable, "-B", "scripts/workflow.py", "suite", "--suite", f"scripts/{suite}"])
    if packaging:
        commands.append(["pwsh", "-NoProfile", "-File", "scripts/package_release_checks.ps1"])
    commands.extend(rust_checks)
    return commands, sorted(set(manual))


def display(command: list[str]) -> str:
    # For display only; all execution uses argument arrays and shell=False.
    return subprocess.list2cmdline(command)


def whitespace(root: Path, paths: list[str], base: str) -> int:
    if not paths:
        return 0
    commands = [["git", "--literal-pathspecs", "diff", "--check", base, "--", *paths]]
    tracked = set(records(git(root, "ls-files", "--cached", "-z", "--", *paths)))
    # A normal Git diff omits untracked files, including explicit ignored inputs.
    # This read-only comparison cannot convert files; suppress conversion warnings.
    commands.extend(["git", "-c", "core.safecrlf=false", "diff", "--no-index", "--check", "--", os.devnull, path]
                    for path in paths if path not in tracked and (root / path).is_file())
    failed = False
    for command in commands:
        result = subprocess.run(command, cwd=root, capture_output=True)
        for stream in (result.stdout, result.stderr):
            if stream:
                print(stream.decode("utf-8", errors="replace").rstrip())
        # --no-index implies --exit-code: 1 with no diagnostics is a clean diff.
        clean_difference = ("--no-index" in command and result.returncode == 1
                            and not result.stdout and not result.stderr)
        failed |= result.returncode != 0 and not clean_difference
    return 1 if failed else 0


def input_hashes(root: Path, paths: list[str]) -> dict[str, str | None]:
    hashes: dict[str, str | None] = {}
    for path in paths:
        candidate = root / path
        if candidate.is_file():
            with candidate.open("rb") as stream:
                hashes[path] = hashlib.file_digest(stream, "sha256").hexdigest()
        else:
            hashes[path] = None
    return hashes


def syntax(root: Path, paths: list[str]) -> int:
    for path in paths:
        if not path.endswith(".py") or not (root / path).is_file():
            raise ValueError(f"Syntax check requires an existing Python file: {path}")
        compile((root / path).read_bytes(), path, "exec")
    print(f"PASS: Python syntax for {len(paths)} files; no bytecode written.")
    return 0


def run_suite(root: Path, path: str) -> int:
    candidate = (root / path).resolve()
    if not candidate.is_relative_to(root.resolve()) or not candidate.is_file():
        raise ValueError(f"Test suite must be an existing repository file: {path}")
    loader = unittest.TestLoader()
    suite = loader.discover(str(candidate.parent), pattern=candidate.name,
                            top_level_dir=str(candidate.parent))
    if not suite.countTestCases():
        raise ValueError(f"Test suite collected zero tests: {path}")
    return 0 if unittest.TextTestRunner().run(suite).wasSuccessful() else 1


def execute(root: Path, command: list[str], stream, timeout: int) -> int:
    options = {"creationflags": subprocess.CREATE_NEW_PROCESS_GROUP} if os.name == "nt" else {"start_new_session": True}
    process = subprocess.Popen(command, cwd=root, stdout=stream, stderr=subprocess.STDOUT, **options)
    try:
        return process.wait(timeout=timeout)
    except (subprocess.TimeoutExpired, KeyboardInterrupt):
        if process.poll() is None:
            if os.name == "nt":
                try:
                    subprocess.run(["taskkill", "/PID", str(process.pid), "/T", "/F"],
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                   timeout=10, check=False)
                except (OSError, subprocess.TimeoutExpired):
                    pass
            else:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            if process.poll() is None:
                process.kill()
        process.wait(timeout=10)
        raise


def run_checks(root: Path, commands: list[list[str]], paths: list[str], base: str,
               pending: list[str] | None = None, timeout: int = DEFAULT_TIMEOUT) -> int:
    start_run = time.monotonic()
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    evidence = root / "work" / "workflow" / f"{stamp}-{uuid.uuid4().hex[:8]}"
    evidence.mkdir(parents=True, exist_ok=False)
    receipt = {
        "started_utc": stamp, "base": base, "paths": paths, "commands": [],
        "scope": "selected checks only; documents receive whitespace and applicable policy structure checks, not content validation; no gameplay or artifact promotion",
        "per_command_timeout_seconds": timeout,
        "pending_manual": pending or [], "input_sha256": input_hashes(root, paths),
        "fingerprint_scope": "selected files only, not all dependencies or environment; no validation cache",
        "status": "running",
    }
    receipt_path = evidence / "receipt.json"

    def save() -> None:
        temporary = receipt_path.with_suffix(".tmp")
        temporary.write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8")
        temporary.replace(receipt_path)

    def finish(status: str) -> None:
        receipt["status"] = status
        receipt["finished_utc"] = datetime.now(timezone.utc).isoformat()
        receipt["seconds"] = round(time.monotonic() - start_run, 3)
        save()

    save()
    for index, command in enumerate(commands, 1):
        log = evidence / f"{index:02d}.log"
        print(f"[{index}/{len(commands)}] {display(command)}", flush=True)
        start = time.monotonic()
        interrupted = False
        timed_out = False
        try:
            with log.open("wb") as stream:
                code = execute(root, command, stream, timeout)
        except OSError as exc:
            log.write_text(str(exc) + "\n", encoding="utf-8")
            code = 127
        except KeyboardInterrupt:
            code = 130
            interrupted = True
        except subprocess.TimeoutExpired:
            code = 124
            timed_out = True
        receipt["commands"].append({
            "argv": command, "exit_code": code,
            "seconds": round(time.monotonic() - start, 3), "log": log.name,
        })
        if interrupted:
            finish("interrupted")
            print(f"\nInterrupted. Evidence: {evidence}", flush=True)
            return 130
        if code:
            finish("timed_out" if timed_out else "failed")
            with log.open(encoding="utf-8", errors="replace") as stream:
                print("".join(deque(stream, maxlen=40)).rstrip())
            print(f"FAILED (exit {code}). Evidence: {evidence}")
            return 1
        save()
    try:
        after = input_hashes(root, paths)
    except OSError as exc:
        receipt["input_error"] = str(exc)
        finish("failed")
        print(f"FAILED: cannot verify final inputs: {exc}. Evidence: {evidence}")
        return 1
    drift = [path for path in paths if receipt["input_sha256"][path] != after[path]]
    if drift:
        receipt["changed_during_checks"] = drift
        receipt["final_input_sha256"] = after
        finish("stale")
        print("STALE: selected files changed during checks:\n  " + "\n  ".join(drift))
        print(f"Evidence: {evidence}")
        return 3
    if pending:
        finish("incomplete")
        print(f"INCOMPLETE: {len(commands)} checks passed; {len(pending)} files still need manual verification.")
        print(f"Evidence: {evidence}")
        return 2
    finish("passed")
    print(f"PASS: {len(commands)} selected checks. Evidence: {evidence}")
    return 0


def brief(root: Path) -> None:
    manifest = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    version = manifest["workspace"]["package"]["version"]
    head = git(root, "rev-parse", "--short", "HEAD").strip()
    branch = git(root, "branch", "--show-current").strip() or "(detached)"
    paths = changed_paths(root)
    print(f"Source: {version} | Git: {branch} @ {head} | Changed files: {len(paths)}")
    current = (root / "CURRENT.md").read_text(encoding="utf-8")
    print("\n" + "\n".join(current.splitlines()[:70]))
    if len(current.splitlines()) > 70:
        print("\nCURRENT.md exceeds 70 lines: compact it before the next handoff.")
    recorded = next((line for line in current.splitlines() if line.startswith("- Source:")), "")
    if version not in recorded.split():
        print("\nCHECKPOINT DRIFT: update CURRENT.md to match the source version.")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("brief", "plan", "check", "whitespace", "syntax", "suite"))
    parser.add_argument("--path", action="append", default=[], help="select a repository file/directory; repeatable")
    parser.add_argument("--base", default="HEAD", help="compare with this commit/ref (default: HEAD)")
    parser.add_argument("--suite", help="exact repository test file, for suite only")
    parser.add_argument("--timeout", type=int, default=DEFAULT_TIMEOUT,
                        help="maximum seconds per check (1-3600; default: 900)")
    args = parser.parse_args(argv)
    try:
        if args.base.startswith("-"):
            raise ValueError("--base must be a Git commit or ref, not an option")
        if not 1 <= args.timeout <= 3600:
            raise ValueError("--timeout must be between 1 and 3600 seconds")
        if args.command == "suite":
            if not args.suite or args.path:
                raise ValueError("suite requires --suite and does not accept --path")
            return run_suite(ROOT, args.suite)
        if args.suite:
            raise ValueError("--suite is only accepted by suite")
        if args.command == "brief":
            if args.path or args.base != "HEAD":
                raise ValueError("brief does not accept --path or --base; use plan")
            brief(ROOT)
            return 0
        paths = select_paths(ROOT, args.path, args.base)
        if args.command == "syntax":
            return syntax(ROOT, paths)
        if args.command == "whitespace":
            return whitespace(ROOT, paths, args.base)
        commands, manual = make_plan(ROOT, paths, args.base)
        print(f"Scope: {len(paths)} files; {'explicit selection' if args.path else 'all changes'} against {args.base}")
        if not commands:
            print("No changed files. No checks executed.")
            return 0
        if manual:
            print("Manual check selection required for:\n  " + "\n  ".join(manual))
        if args.command == "plan":
            for command in commands:
                print("  " + display(command))
            print("Preview only. No checks executed.")
            return 0
        return run_checks(ROOT, commands, paths, args.base, manual, args.timeout)
    except (OSError, ValueError, RuntimeError, SyntaxError, subprocess.SubprocessError) as exc:
        print(f"workflow: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
