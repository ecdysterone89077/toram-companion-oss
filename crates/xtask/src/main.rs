//! Pure Rust workspace automation, build pipeline, staging, and lifecycle manager (`cargo xtask`).
//!
//! Replaces all legacy shell, batch, and PowerShell scripts with a single 100% pure Rust toolchain.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("help");

    let res = match cmd {
        "build" => cmd_build(&args[1..]),
        "run" => cmd_run(&args[1..]),
        "test" => cmd_test(&args[1..]),
        "hash" => cmd_hash(&args[1..]),
        "clean" => cmd_clean(&args[1..]),
        "help" | "-h" | "--help" => {
            print_help();
            Ok(())
        }
        unknown => {
            eprintln!("[xtask] Unknown command: '{unknown}'. Run 'cargo xtask help' for available commands.");
            Err(())
        }
    };

    if res.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn print_help() {
    println!(
        r#"toram-companion xtask — 100% Pure Rust Workspace Manager

Usage: cargo xtask <command> [options]

Commands:
    build [--no-test] [--stage-dir DIR]
                        Test, build and stage all four release artifacts with SHA-256
    run [bin] [-- ARGS]  Test, build and launch companion, wboot or observer
    test                Run full workspace unit and integration test suite
    hash [--stage-dir DIR] Calculate SHA-256 of all staged artifacts
    clean               Clean target directory and intermediate artifacts
    help                Show this help message
"#
    );
}

fn project_root() -> Result<PathBuf, ()> {
    let start = env::current_dir().map_err(|e| eprintln!("[xtask] Current directory: {e}"))?;
    project_root_from(&start)
}

fn project_root_from(start: &Path) -> Result<PathBuf, ()> {
    let output = Command::new("cargo")
        .args(["locate-project", "--workspace", "--message-format", "plain"])
        .current_dir(start)
        .output()
        .map_err(|e| eprintln!("[xtask] Cannot locate workspace: {e}"))?;
    if !output.status.success() {
        eprintln!(
            "[xtask] Cannot locate workspace: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        return Err(());
    }
    let manifest = String::from_utf8(output.stdout)
        .map_err(|e| eprintln!("[xtask] Invalid workspace path: {e}"))?;
    Path::new(manifest.trim())
        .parent()
        .map(Path::to_path_buf)
        .filter(|path| path.join("Cargo.toml").is_file())
        .ok_or_else(|| eprintln!("[xtask] Cargo returned an invalid workspace manifest"))
}

fn run_command(prog: &str, args: &[&str]) -> Result<(), ()> {
    println!("[xtask] > {} {}", prog, args.join(" "));
    let status = Command::new(prog)
        .args(args)
        .current_dir(project_root()?)
        .status()
        .map_err(|e| {
            eprintln!("[xtask] Failed to execute {prog}: {e}");
        })?;

    if status.success() {
        Ok(())
    } else {
        eprintln!("[xtask] Command failed with exit code: {:?}", status.code());
        Err(())
    }
}

fn cmd_test(args: &[String]) -> Result<(), ()> {
    reject_extra_args(args)?;
    println!("[xtask] Running workspace unit & integration tests...");
    run_command("cargo", &["test", "--workspace", "--locked", "--offline"])
}

fn stage_options(args: &[String], root: &Path, allow_skip: bool) -> Result<(PathBuf, bool), ()> {
    let mut destination = root.to_path_buf();
    let mut skip = false;
    let mut seen_destination = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--no-test" if allow_skip && !skip => skip = true,
            "--stage-dir" if !seen_destination => {
                destination = args
                    .next()
                    .filter(|p| !p.starts_with("--"))
                    .map(PathBuf::from)
                    .ok_or_else(|| eprintln!("[xtask] --stage-dir requires a directory"))?;
                seen_destination = true;
            }
            other => {
                eprintln!("[xtask] Unknown or duplicate option: {other}");
                return Err(());
            }
        }
    }
    Ok((destination, skip))
}

const ARTIFACTS: [(&str, &str); 4] = [
    ("companion.exe", "companion.exe"),
    ("wcore.dll", "src/wcore.dll"),
    ("wboot.exe", "src/wboot.exe"),
    ("observer.exe", "observer.exe"),
];

fn cmd_build(args: &[String]) -> Result<(), ()> {
    let root = project_root()?;
    let (stage, skip_tests) = stage_options(args, &root, true)?;

    if !skip_tests {
        println!("[xtask] [1/3] Running workspace unit & integration tests...");
        run_command("cargo", &["test", "--workspace", "--locked", "--offline"])?;
    } else {
        println!("[xtask] [1/3] Skipping tests (--no-test requested)");
    }

    println!("[xtask] [2/3] Building LTO-optimized release binaries...");
    run_command(
        "cargo",
        &[
            "build",
            "--release",
            "--workspace",
            "--locked",
            "--offline",
            "--target",
            "x86_64-pc-windows-msvc",
            "--target-dir",
            "target",
        ],
    )?;

    println!("[xtask] Verifying the optimized worker in an isolated process...");
    run_command(
        "cargo",
        &[
            "run",
            "--package",
            "wcore",
            "--example",
            "worker_lifecycle",
            "--release",
            "--locked",
            "--offline",
            "--target",
            "x86_64-pc-windows-msvc",
            "--target-dir",
            "target",
        ],
    )?;

    println!(
        "[xtask] [3/3] Staging release artifacts in {}...",
        stage.display()
    );

    let rel_dir = root.join("target/x86_64-pc-windows-msvc/release");

    for (source, _) in ARTIFACTS {
        if !rel_dir.join(source).is_file() {
            eprintln!("[xtask] Missing release artifact: {source}");
            return Err(());
        }
    }
    let src_dir = stage.join("src");
    fs::create_dir_all(&src_dir).map_err(|e| {
        eprintln!("[xtask] Failed to create src directory: {e}");
    })?;

    let mut json = String::from("[\n");
    for (i, (source, rel_name)) in ARTIFACTS.iter().enumerate() {
        let source = rel_dir.join(source);
        let destination = stage.join(rel_name);
        copy_file(&source, &destination)?;
        let hash = sha256_file(&destination)?;
        if hash != sha256_file(&source)? {
            eprintln!("[xtask] Staged artifact checksum mismatch: {rel_name}");
            return Err(());
        }
        println!("[xtask] Staged: {:<16} SHA256: {}", rel_name, hash);
        json.push_str("    {\n");
        json.push_str(&format!("        \"File\":  \"{}\",\n", rel_name));
        json.push_str(&format!("        \"SHA256\":  \"{}\"\n", hash));
        if i + 1 < ARTIFACTS.len() {
            json.push_str("    },\n");
        } else {
            json.push_str("    }\n");
        }
    }
    json.push_str("]\n");

    let sha_path = stage.join("sha256.json");
    fs::write(&sha_path, json).map_err(|e| {
        eprintln!("[xtask] Failed to write sha256.json: {e}");
    })?;

    println!("[xtask] Build and staging complete. Updated sha256.json.");
    Ok(())
}

fn cmd_run(args: &[String]) -> Result<(), ()> {
    let root = project_root()?;
    let target_bin = args.first().map(|s| s.as_str()).unwrap_or("companion");

    let exe_path = match target_bin {
        "companion" => root.join("companion.exe"),
        "wboot" => root.join("src/wboot.exe"),
        "observer" => root.join("observer.exe"),
        other => {
            eprintln!("[xtask] Unknown executable: {other}");
            return Err(());
        }
    };
    let forwarded = if args.len() > 1 {
        if args[1] != "--" {
            eprintln!("[xtask] Put executable arguments after --");
            return Err(());
        }
        &args[2..]
    } else {
        &[]
    };
    // Existing binaries may predate the current source repairs.
    cmd_build(&[])?;

    println!("[xtask] Launching: {}", exe_path.display());
    let status = Command::new(&exe_path)
        .args(forwarded)
        .current_dir(&root)
        .status()
        .map_err(|e| {
            eprintln!("[xtask] Failed to spawn {}: {e}", exe_path.display());
        })?;
    if !status.success() {
        eprintln!("[xtask] Executable exited with {status}");
        return Err(());
    }

    Ok(())
}

fn cmd_hash(args: &[String]) -> Result<(), ()> {
    let (stage, _) = stage_options(args, &project_root()?, false)?;
    for (_, name) in ARTIFACTS {
        let path = stage.join(name);
        if path.is_file() {
            let hash = sha256_file(&path)?;
            let meta = fs::metadata(&path).map_err(|_| ())?;
            println!(
                "{:<16} size: {:>8} bytes  SHA256: {}",
                name,
                meta.len(),
                hash
            );
        } else {
            println!("{:<16} MISSING", name);
            return Err(());
        }
    }
    Ok(())
}

fn reject_extra_args(args: &[String]) -> Result<(), ()> {
    if !args.is_empty() {
        eprintln!("[xtask] Unexpected options: {}", args.join(" "));
        return Err(());
    }
    Ok(())
}

fn cmd_clean(args: &[String]) -> Result<(), ()> {
    reject_extra_args(args)?;
    println!("[xtask] Cleaning build artifacts...");
    run_command("cargo", &["clean"])
}

fn copy_file(from: &Path, to: &Path) -> Result<(), ()> {
    if !from.is_file() {
        eprintln!("[xtask] Source file not found: {}", from.display());
        return Err(());
    }
    for attempt in 1..=5 {
        match fs::copy(from, to) {
            Ok(_) => return Ok(()),
            Err(_e) if attempt < 5 => {
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
            Err(e) => {
                eprintln!(
                    "[xtask] Failed to copy {} -> {}: {e}",
                    from.display(),
                    to.display()
                );
                return Err(());
            }
        }
    }
    Ok(())
}

/// Standalone pure Rust SHA-256 implementation (FIPS 180-4 standard).
/// Zero external crate dependencies.
fn sha256_file(path: &Path) -> Result<String, ()> {
    let bytes = fs::read(path).map_err(|e| {
        eprintln!("[xtask] Failed to read {}: {e}", path.display());
    })?;
    Ok(sha256_digest(&bytes))
}

fn sha256_digest(data: &[u8]) -> String {
    let k: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (i, part) in chunk.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*part);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let mut a = h[0];
        let mut b = h[1];
        let mut c = h[2];
        let mut d = h[3];
        let mut e = h[4];
        let mut f = h[5];
        let mut g = h[6];
        let mut h_val = h[7];

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h_val
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(k[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h_val = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(h_val);
    }

    let mut out = String::with_capacity(64);
    for val in h {
        out.push_str(&format!("{:08X}", val));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_root_is_found_from_a_member_crate() {
        let member = Path::new(env!("CARGO_MANIFEST_DIR"));
        let expected = member.parent().unwrap().parent().unwrap();
        assert_eq!(project_root_from(member).unwrap(), expected);
        assert_eq!(project_root_from(&member.join("src")).unwrap(), expected);
    }

    #[test]
    fn stage_options_are_explicit_and_include_the_observer() {
        let root = Path::new("project");
        let args = ["--stage-dir", "release", "--no-test"].map(str::to_owned);
        assert_eq!(
            stage_options(&args, root, true).unwrap(),
            (PathBuf::from("release"), true)
        );
        assert!(stage_options(&["--stage-dir".into()], root, true).is_err());
        assert!(stage_options(&["--no-test".into()], root, false).is_err());
        assert_eq!(ARTIFACTS.len(), 4);
        assert!(ARTIFACTS.contains(&("observer.exe", "observer.exe")));
        assert!(ARTIFACTS.iter().all(|(_, name)| !name.contains('\\')));
    }

    #[test]
    fn release_hash_matches_standard_vectors() {
        assert_eq!(
            sha256_digest(b"").to_ascii_lowercase(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_digest(b"abc").to_ascii_lowercase(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
