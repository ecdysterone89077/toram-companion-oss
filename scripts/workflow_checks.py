"""Regression checks for scope selection and fail-fast execution; no game needed."""

from contextlib import redirect_stdout
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import workflow as wf


DIRECTORIES = {f"crates/{name}": name for name in
               ("protocol", "wcore", "wboot", "companion", "observer", "xtask")}
DEPENDENCIES = {
    "protocol": set(), "wcore": {"protocol"}, "wboot": {"protocol"},
    "companion": {"protocol", "wboot"}, "observer": {"protocol", "wboot"},
    "xtask": set(),
}


class Planning(unittest.TestCase):
    def plan(self, *paths):
        with patch.object(wf, "cargo_graph", return_value=(DIRECTORIES, DEPENDENCIES)):
            return wf.make_plan(wf.ROOT, list(paths))

    def test_documentation_never_loads_cargo(self):
        with patch.object(wf, "cargo_graph", side_effect=AssertionError("unexpected Cargo")):
            commands, manual = wf.make_plan(wf.ROOT, ["docs/new.md"])
        self.assertEqual(len(commands), 1)
        self.assertEqual(commands[0][:3], [sys.executable, "scripts/workflow.py", "whitespace"])
        self.assertFalse(manual)

    def test_menu_change_stays_in_menu(self):
        commands, manual = self.plan("crates/companion/src/main.rs")
        test = next(cmd for cmd in commands if cmd[:2] == ["cargo", "test"])
        self.assertEqual(test, ["cargo", "test", "-p", "companion", "--locked", "--offline"])
        self.assertIn(["cargo", "fmt", "-p", "companion", "--", "--check"], commands)
        self.assertFalse(manual)

    def test_loader_change_includes_consumers(self):
        commands, _ = self.plan("crates/wboot/src/lib.rs")
        test = next(cmd for cmd in commands if cmd[:2] == ["cargo", "test"])
        self.assertEqual(test[2:-2], ["-p", "companion", "-p", "observer", "-p", "wboot"])

    def test_dependency_expansion_is_transitive(self):
        self.assertEqual(wf.dependents({"a"}, {"a": set(), "b": {"a"}, "c": {"b"}}),
                         {"a", "b", "c"})

    def test_protocol_and_build_inputs_expand_workspace(self):
        for path in ("crates/protocol/src/ipc.rs", "Cargo.lock", ".cargo/config.toml",
                     "crates/wcore/Cargo.toml", "rust-toolchain.toml"):
            with self.subTest(path=path):
                commands, manual = self.plan(path)
                self.assertIn(["cargo", "test", "--workspace", "--locked", "--offline"], commands)
                self.assertIn(["cargo", "fmt", "--all", "--", "--check"], commands)
                self.assertFalse(manual)

    def test_fast_script_checks_precede_rust(self):
        commands, _ = self.plan("crates/wcore/src/faunas.rs", "scripts/workflow.py")
        last_python = max(i for i, cmd in enumerate(commands) if "suite" in cmd)
        first_rust = next(i for i, cmd in enumerate(commands) if cmd[0] == "cargo")
        self.assertLess(last_python, first_rust)

    def test_audit_suite_is_deduplicated(self):
        commands, manual = self.plan("scripts/faunas_target_audit.py",
                                     "scripts/faunas_target_audit_checks.py",
                                     "crates/wcore/src/faunas.rs")
        self.assertEqual(sum("suite" in cmd and cmd[-1] == "scripts/faunas_target_audit_checks.py"
                             for cmd in commands), 1)
        self.assertFalse(manual)

    def test_unknown_executable_is_not_silently_verified(self):
        _, manual = self.plan("scripts/unmapped.py", ".github/workflows/build.yml")
        self.assertEqual(manual, [".github/workflows/build.yml", "scripts/unmapped.py"])

    def test_packager_has_its_own_fixture_checks(self):
        commands, manual = self.plan("package-release.ps1")
        self.assertIn(["pwsh", "-NoProfile", "-File", "scripts/package_release_checks.ps1"], commands)
        self.assertFalse(manual)

    def test_empty_scope_has_no_commands(self):
        self.assertEqual(self.plan(), ([], []))

    def test_documentation_and_formatting_metadata_need_only_whitespace(self):
        for path in ("AGENTS.md", "WORKFLOW.md", "CURRENT.md", ".editorconfig", ".gitattributes"):
            with self.subTest(path=path):
                commands, manual = self.plan(path)
                self.assertEqual(len(commands), 1)
                self.assertEqual(commands[0][:3], [sys.executable, "scripts/workflow.py", "whitespace"])
                self.assertFalse(manual)

    def test_workflow_changes_use_shipped_suite_and_syntax(self):
        commands, manual = self.plan("scripts/workflow.py", "scripts/workflow_checks.py")
        self.assertEqual({Path(cmd[-1]).name for cmd in commands if "suite" in cmd}, {"workflow_checks.py"})
        syntax = next(cmd for cmd in commands if "syntax" in cmd)
        self.assertEqual(syntax.count("--path"), 2)
        self.assertFalse(manual)
        for command in commands:
            for argument in command:
                if argument.startswith("scripts/"):
                    self.assertTrue((wf.ROOT / argument).is_file(), argument)

    def test_deleted_python_input_is_manual_even_with_surviving_suite(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "scripts").mkdir()
            (root / "scripts/deleted_checks.py").write_text("", encoding="utf-8")
            _, manual = wf.make_plan(root, ["scripts/deleted.py"])
            self.assertEqual(manual, ["scripts/deleted.py"])

    def test_unmapped_python_still_gets_syntax_check(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "standalone.py").write_text("pass\n", encoding="utf-8")
            commands, manual = wf.make_plan(root, ["standalone.py"])
            self.assertTrue(any("syntax" in cmd for cmd in commands))
            self.assertEqual(manual, ["standalone.py"])

    def test_cargo_graph_uses_workspace_metadata(self):
        payload = {
            "workspace_members": ["id:a", "id:b"],
            "packages": [
                {"id": "id:a", "name": "a", "manifest_path": str(wf.ROOT / "crates/a/Cargo.toml"),
                 "dependencies": []},
                {"id": "id:b", "name": "b", "manifest_path": str(wf.ROOT / "crates/b/Cargo.toml"),
                 "dependencies": [{"name": "a"}, {"name": "external"}]},
            ],
        }
        with patch.object(wf, "output", return_value=json.dumps(payload)):
            directories, dependencies = wf.cargo_graph(wf.ROOT)
        self.assertEqual(directories, {"crates/a": "a", "crates/b": "b"})
        self.assertEqual(dependencies, {"a": set(), "b": {"a"}})


class Brief(unittest.TestCase):
    def render(self, checkpoint=None):
        with tempfile.TemporaryDirectory() as tmp, redirect_stdout(io.StringIO()) as messages:
            root = Path(tmp)
            (root / "Cargo.toml").write_text('[workspace.package]\nversion = "1.2.3"\n', encoding="utf-8")
            if checkpoint is not None:
                (root / "CURRENT.md").write_text(checkpoint, encoding="utf-8")
            with patch.object(wf, "git", side_effect=["abc123\n", "main\n"]), \
                    patch.object(wf, "changed_paths", return_value=[]):
                wf.brief(root)
            return messages.getvalue()

    def test_fresh_clone_without_local_checkpoint_still_shows_metadata(self):
        output = self.render()
        self.assertIn("Source: 1.2.3 | Git: main @ abc123 | Changed files: 0", output)
        self.assertIn("No local CURRENT.md checkpoint", output)

    def test_existing_checkpoint_is_shown_and_version_checked(self):
        self.assertNotIn("CHECKPOINT DRIFT", self.render("- Source: 1.2.3\n"))
        output = self.render("- Source: 1.2.2\n")
        self.assertIn("- Source: 1.2.2", output)
        self.assertIn("CHECKPOINT DRIFT", output)


class GitSelection(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()
        self.git("init", "-q")
        self.git("config", "user.email", "workflow-fixture@example.invalid")
        self.git("config", "user.name", "Workflow Fixture")
        self.git("config", "commit.gpgsign", "false")
        (self.root / "old name.rs").write_text("baseline\n", encoding="utf-8")
        (self.root / ".gitignore").write_text("ignored/\n", encoding="utf-8")
        self.git("add", ".")
        self.git("commit", "-qm", "fixture")

    def git(self, *args):
        return wf.git(self.root, *args)

    def test_staged_rename_unstaged_and_untracked_are_all_seen(self):
        self.git("mv", "old name.rs", "new name.rs")
        (self.root / ".gitignore").write_text("ignored/\nextra/\n", encoding="utf-8")
        (self.root / "new file.md").write_text("new\n", encoding="utf-8")
        (self.root / "ignored").mkdir()
        (self.root / "ignored/large.bin").write_bytes(b"excluded")
        self.assertEqual(wf.changed_paths(self.root),
                         [".gitignore", "new file.md", "new name.rs", "old name.rs"])

    def test_base_includes_committed_work(self):
        (self.root / "old name.rs").write_text("committed change\n", encoding="utf-8")
        self.git("commit", "-qam", "change")
        self.assertEqual(wf.changed_paths(self.root), [])
        self.assertEqual(wf.changed_paths(self.root, "HEAD~1"), ["old name.rs"])

    def test_explicit_scope_excludes_unrelated_changes(self):
        (self.root / "other.md").write_text("other\n", encoding="utf-8")
        self.assertEqual(wf.select_paths(self.root, ["old name.rs"], "HEAD"), ["old name.rs"])

    def test_literal_directory_selection(self):
        (self.root / "folder [x]").mkdir()
        (self.root / "folder [x]/new.md").write_text("new\n", encoding="utf-8")
        self.assertEqual(wf.select_paths(self.root, ["folder [x]"], "HEAD"),
                         ["folder [x]/new.md"])

    def test_explicit_scope_keeps_staged_deletions(self):
        self.git("rm", "old name.rs")
        self.assertEqual(wf.select_paths(self.root, ["old name.rs"], "HEAD"), ["old name.rs"])

    def test_bad_paths_and_refs_fail(self):
        for path in ("../outside", "missing.file"):
            with self.subTest(path=path), self.assertRaises(ValueError):
                wf.select_paths(self.root, [path], "HEAD")
        with self.assertRaises(ValueError):
            wf.changed_paths(self.root, "--no-index")

    def test_new_document_whitespace_is_checked_with_literal_filename(self):
        path = "new [x] document.md"
        (self.root / path).write_text("bad trailing space \n", encoding="utf-8")
        with redirect_stdout(io.StringIO()) as result:
            code = wf.whitespace(self.root, [path], "HEAD")
        self.assertEqual(code, 1)
        self.assertIn("trailing whitespace", result.getvalue())

    def test_tracked_diff_whitespace_is_still_checked(self):
        (self.root / "old name.rs").write_text("new bad text \n", encoding="utf-8")
        with redirect_stdout(io.StringIO()):
            self.assertEqual(wf.whitespace(self.root, ["old name.rs"], "HEAD"), 1)

    def test_clean_new_file_and_deleted_file_pass(self):
        (self.root / "clean.md").write_text("clean\n", encoding="utf-8")
        self.git("rm", "old name.rs")
        with redirect_stdout(io.StringIO()):
            self.assertEqual(wf.whitespace(self.root, ["clean.md", "old name.rs"], "HEAD"), 0)

    def test_explicit_ignored_file_is_checked(self):
        (self.root / "ignored").mkdir()
        (self.root / "ignored/doc.md").write_text("trailing \n", encoding="utf-8")
        with redirect_stdout(io.StringIO()):
            self.assertEqual(wf.whitespace(self.root, ["ignored/doc.md"], "HEAD"), 1)

    def test_windows_line_ending_warning_does_not_fail_a_clean_new_file(self):
        self.git("config", "core.autocrlf", "true")
        self.git("config", "core.safecrlf", "warn")
        (self.root / "new.md").write_bytes(b"clean\n")
        with redirect_stdout(io.StringIO()):
            self.assertEqual(wf.whitespace(self.root, ["new.md"], "HEAD"), 0)
        (self.root / "new.md").write_bytes(b"still bad \n")
        with redirect_stdout(io.StringIO()):
            self.assertEqual(wf.whitespace(self.root, ["new.md"], "HEAD"), 1)
        self.assertEqual(self.git("config", "--get", "core.safecrlf").strip(), "warn")

    def test_git_read_error_is_not_mistaken_for_a_clean_new_file(self):
        (self.root / "new.md").write_text("clean\n", encoding="utf-8")
        results = [subprocess.CompletedProcess([], 0, b"", b""),
                   subprocess.CompletedProcess([], 1, b"", b"error: cannot read new.md")]
        with redirect_stdout(io.StringIO()), patch.object(wf, "git", return_value=""), \
                patch.object(wf.subprocess, "run", side_effect=results):
            self.assertEqual(wf.whitespace(self.root, ["new.md"], "HEAD"), 1)


class Execution(unittest.TestCase):
    def test_empty_existing_suite_cannot_pass(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "empty_fixture_checks.py").write_text("# no tests\n", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "zero tests"):
                wf.run_suite(root, "empty_fixture_checks.py")

    def test_missing_suite_cannot_pass(self):
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaisesRegex(ValueError, "existing repository file"):
                wf.run_suite(Path(tmp), "missing_checks.py")

    def test_python_syntax_check_does_not_create_bytecode(self):
        with tempfile.TemporaryDirectory() as tmp, redirect_stdout(io.StringIO()):
            root = Path(tmp)
            (root / "valid.py").write_text("def answer():\n    return 42\n", encoding="utf-8")
            self.assertEqual(wf.syntax(root, ["valid.py"]), 0)
            self.assertFalse((root / "__pycache__").exists())
            (root / "invalid.py").write_text("def broken(:\n", encoding="utf-8")
            with self.assertRaises(SyntaxError):
                wf.syntax(root, ["invalid.py"])
            with self.assertRaises(ValueError):
                wf.syntax(root, ["missing.py"])

    def test_unknown_files_do_not_block_mapped_checks_or_claim_success(self):
        with tempfile.TemporaryDirectory() as tmp, redirect_stdout(io.StringIO()) as messages:
            root = Path(tmp)
            (root / "unknown.py").write_text("pass\n", encoding="utf-8")
            commands = [[sys.executable, "-c", "print('mapped check ran')"]]
            with patch.object(wf, "ROOT", root), \
                    patch.object(wf, "select_paths", return_value=["unknown.py"]), \
                    patch.object(wf, "make_plan", return_value=(commands, ["unknown.py"])):
                self.assertEqual(wf.main(["check", "--path", "unknown.py"]), 2)
            receipt = json.loads(next(root.glob("work/workflow/*/receipt.json")).read_text())
            self.assertEqual(receipt["status"], "incomplete")
            self.assertEqual(receipt["pending_manual"], ["unknown.py"])
            self.assertEqual(receipt["commands"][0]["exit_code"], 0)
            self.assertIn("mapped check ran", next(root.glob("work/workflow/*/01.log")).read_text())
            self.assertNotIn("PASS:", messages.getvalue())

    def test_plan_does_not_execute_even_with_pending_files(self):
        with redirect_stdout(io.StringIO()), \
                patch.object(wf, "select_paths", return_value=["unknown.py"]), \
                patch.object(wf, "make_plan", return_value=([["nonexistent"]], ["unknown.py"])), \
                patch.object(wf, "run_checks", side_effect=AssertionError("executed preview")):
            self.assertEqual(wf.main(["plan"]), 0)

    def test_selected_edits_creations_and_deletions_invalidate_pass(self):
        operations = {
            "edit": "Path('input.md').write_text('after')",
            "create": "Path('input.md').write_text('new')",
            "delete": "Path('input.md').unlink()",
        }
        for kind, action in operations.items():
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as tmp, redirect_stdout(io.StringIO()):
                root = Path(tmp)
                if kind != "create":
                    (root / "input.md").write_text("before", encoding="utf-8")
                commands = [[sys.executable, "-c", "from pathlib import Path; " + action]]
                self.assertEqual(wf.run_checks(root, commands, ["input.md"], "HEAD"), 3)
                receipt = json.loads(next(root.glob("work/workflow/*/receipt.json")).read_text())
                self.assertEqual(receipt["status"], "stale")
                self.assertEqual(receipt["changed_during_checks"], ["input.md"])

    def test_interruption_records_the_command_and_stops(self):
        with tempfile.TemporaryDirectory() as tmp, redirect_stdout(io.StringIO()):
            root = Path(tmp)
            with patch.object(wf, "execute", side_effect=KeyboardInterrupt) as run:
                self.assertEqual(wf.run_checks(root, [["first"], ["second"]], [], "HEAD"), 130)
                self.assertEqual(run.call_count, 1)
            receipt = json.loads(next(root.glob("work/workflow/*/receipt.json")).read_text())
            self.assertEqual(receipt["status"], "interrupted")
            self.assertEqual(receipt["commands"][0]["exit_code"], 130)
            self.assertIn("finished_utc", receipt)

    def test_unreadable_final_input_records_failure_instead_of_running_forever(self):
        with tempfile.TemporaryDirectory() as tmp, redirect_stdout(io.StringIO()):
            root = Path(tmp)
            with patch.object(wf, "input_hashes", side_effect=[{}, OSError("locked input")]):
                self.assertEqual(wf.run_checks(root, [], [], "HEAD"), 1)
            receipt = json.loads(next(root.glob("work/workflow/*/receipt.json")).read_text())
            self.assertEqual(receipt["status"], "failed")
            self.assertEqual(receipt["input_error"], "locked input")

    def test_failure_stops_later_commands_and_records_exit_code(self):
        with tempfile.TemporaryDirectory() as tmp, redirect_stdout(io.StringIO()):
            root = Path(tmp)
            commands = [
                [sys.executable, "-c", "print('intentional failure'); raise SystemExit(7)"],
                [sys.executable, "-c", "from pathlib import Path; Path('should-not-exist').touch()"],
            ]
            self.assertEqual(wf.run_checks(root, commands, ["fixture.md"], "HEAD"), 1)
            self.assertFalse((root / "should-not-exist").exists())
            receipt = json.loads(next(root.glob("work/workflow/*/receipt.json")).read_text())
            self.assertEqual(receipt["status"], "failed")
            self.assertEqual(len(receipt["commands"]), 1)
            self.assertEqual(receipt["commands"][0]["exit_code"], 7)

    def test_reserved_child_exit_codes_remain_failures(self):
        for child_code in (2, 3, 130):
            with self.subTest(child_code=child_code), tempfile.TemporaryDirectory() as tmp, redirect_stdout(io.StringIO()):
                root = Path(tmp)
                with patch.object(wf, "execute", return_value=child_code):
                    self.assertEqual(wf.run_checks(root, [["check"]], [], "HEAD"), 1)
                receipt = json.loads(next(root.glob("work/workflow/*/receipt.json")).read_text())
                self.assertEqual(receipt["status"], "failed")
                self.assertEqual(receipt["commands"][0]["exit_code"], child_code)

    def test_timeout_records_failure_and_stops_later_commands(self):
        with tempfile.TemporaryDirectory() as tmp, redirect_stdout(io.StringIO()):
            root = Path(tmp)
            with patch.object(wf, "execute", side_effect=subprocess.TimeoutExpired("check", 5)) as execute:
                self.assertEqual(wf.run_checks(root, [["check"], ["later"]], [], "HEAD", timeout=5), 1)
            self.assertEqual(execute.call_count, 1)
            receipt = json.loads(next(root.glob("work/workflow/*/receipt.json")).read_text())
            self.assertEqual(receipt["status"], "timed_out")
            self.assertEqual(receipt["per_command_timeout_seconds"], 5)

    def test_execute_terminates_timed_out_process(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with (root / "log.txt").open("wb") as stream, \
                    self.assertRaises(subprocess.TimeoutExpired):
                wf.execute(root, [sys.executable, "-c", "import time; time.sleep(30)"], stream, 1)

    @unittest.skipUnless(sys.platform == "win32", "Windows process-tree cleanup")
    def test_windows_timeout_terminates_descendant(self):
        import ctypes

        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel.OpenProcess.argtypes = [ctypes.c_uint32, ctypes.c_int, ctypes.c_uint32]
        kernel.OpenProcess.restype = ctypes.c_void_p
        kernel.WaitForSingleObject.argtypes = [ctypes.c_void_p, ctypes.c_uint32]
        kernel.WaitForSingleObject.restype = ctypes.c_uint32
        kernel.TerminateProcess.argtypes = [ctypes.c_void_p, ctypes.c_uint32]
        kernel.TerminateProcess.restype = ctypes.c_int
        kernel.CloseHandle.argtypes = [ctypes.c_void_p]
        kernel.CloseHandle.restype = ctypes.c_int
        parent = ("import subprocess, sys, time\nfrom pathlib import Path\n"
                  "child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'])\n"
                  "Path('descendant.pid').write_text(str(child.pid))\ntime.sleep(30)\n")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            with (root / "log.txt").open("wb") as stream, self.assertRaises(subprocess.TimeoutExpired):
                wf.execute(root, [sys.executable, "-c", parent], stream, 1)
            pid = int((root / "descendant.pid").read_text())
            handle = kernel.OpenProcess(0x100001, False, pid)  # SYNCHRONIZE | PROCESS_TERMINATE
            if handle:
                try:
                    state = kernel.WaitForSingleObject(handle, 0)
                    if state != 0:
                        kernel.TerminateProcess(handle, 1)
                        kernel.WaitForSingleObject(handle, 5000)
                    self.assertEqual(state, 0, "descendant survived timeout cleanup")
                finally:
                    kernel.CloseHandle(handle)
            else:
                self.assertEqual(ctypes.get_last_error(), 87)  # PID already reaped.

    def test_timeout_bounds_are_rejected(self):
        for value in (0, -1, 3601):
            with self.subTest(value=value), patch.object(wf, "select_paths") as select:
                self.assertEqual(wf.main(["check", "--timeout", str(value)]), 1)
                select.assert_not_called()

    def test_success_is_logged(self):
        with tempfile.TemporaryDirectory() as tmp, redirect_stdout(io.StringIO()):
            root = Path(tmp)
            self.assertEqual(wf.run_checks(root, [[sys.executable, "-c", "print('ok')"]], [], "HEAD"), 0)
            receipt = json.loads(next(root.glob("work/workflow/*/receipt.json")).read_text())
            self.assertEqual(receipt["status"], "passed")
            self.assertEqual(receipt["pending_manual"], [])
            self.assertGreaterEqual(receipt["seconds"], 0)
            self.assertEqual(next(root.glob("work/workflow/*/01.log")).read_text().strip(), "ok")

    def test_missing_program_is_a_recorded_failure(self):
        with tempfile.TemporaryDirectory() as tmp, redirect_stdout(io.StringIO()):
            root = Path(tmp)
            self.assertNotEqual(wf.run_checks(root, [["missing-workflow-program-123456"]], [], "HEAD"), 0)
            receipt = json.loads(next(root.glob("work/workflow/*/receipt.json")).read_text())
            self.assertEqual(receipt["commands"][0]["exit_code"], 127)


if __name__ == "__main__":
    unittest.main()
