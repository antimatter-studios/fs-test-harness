//! mock-fs -- stand-in CLI for fs-test-harness's mock-scenario CI job.
//!
//! Pretends to be a filesystem driver so the harness's full loop can be
//! exercised end-to-end without needing WinFsp, a real disk image, or
//! an SSH'd VM. Subcommands:
//!
//!   mock-fs mount <image> --drive <letter> [--rw]
//!     Prints a deterministic ready line ("mounted at <letter>") then
//!     blocks on stdin until killed -- mirrors the lifetime of a real
//!     mount that lives until taskkill /T /F tears it down.
//!
//!   mock-fs ls <image> <path>
//!     Prints one entry per line. Top-level (`/`) yields a single
//!     "hello.txt" entry; everything else is empty (unknown path).
//!
//!   mock-fs cat <image> <path>
//!     Prints "hello world\n" for `/hello.txt`, exits non-zero
//!     otherwise.
//!
//!   mock-fs stat <image> <path>
//!     Prints canned key=value lines for `/hello.txt`, exits non-zero
//!     otherwise.
//!
//! Pure std; builds on Linux, macOS, and Windows. Whatever image path
//! is passed is recorded but not opened.

use std::env;
use std::io::{self, Write};
use std::process::ExitCode;

const HELLO_BODY: &str = "hello world\n";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: mock-fs <mount|ls|cat|stat> ...");
        return ExitCode::from(2);
    }
    match args[0].as_str() {
        "mount" => cmd_mount(&args[1..]),
        "ls" => cmd_ls(&args[1..]),
        "cat" => cmd_cat(&args[1..]),
        "stat" => cmd_stat(&args[1..]),
        other => {
            eprintln!("mock-fs: unknown subcommand: {other}");
            ExitCode::from(2)
        }
    }
}

/// Parse `<image> [--drive X] [--rw]` style argv. Tolerant of the
/// extra flags the harness substitutes via `{extra}`.
fn cmd_mount(args: &[String]) -> ExitCode {
    let mut image: Option<&str> = None;
    let mut drive: Option<&str> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--drive" => {
                if i + 1 >= args.len() {
                    eprintln!("mock-fs mount: --drive expects a value");
                    return ExitCode::from(2);
                }
                drive = Some(&args[i + 1]);
                i += 2;
            }
            "--rw" => i += 1,
            other if !other.starts_with("--") && image.is_none() => {
                image = Some(other);
                i += 1;
            }
            other => {
                // Tolerate extra flags so `{extra}` substitution is harmless.
                eprintln!("mock-fs mount: ignoring unknown flag: {other}");
                i += 1;
            }
        }
    }
    let image = match image {
        Some(s) => s,
        None => {
            eprintln!("mock-fs mount: missing <image>");
            return ExitCode::from(2);
        }
    };
    let drive = drive.unwrap_or("Z:");

    // Ready-line first: this is what `run-scenario.ps1` greps for to
    // declare the mount up. Flush so the parent sees it before we
    // start blocking.
    println!("mock-fs: image={image}");
    println!("mounted at {drive}");
    let _ = io::stdout().flush();

    // Block forever — the harness's `taskkill /T /F` (Windows) /
    // SIGTERM (Unix) tears us down at scenario teardown. Don't read
    // from stdin: `Start-Process` on Windows hands the child a closed
    // stdin pipe, so a stdin-coupled wait would EOF instantly and we'd
    // exit before the parent saw the ready_line.
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

fn parse_image_and_path<'a>(
    cmd: &str,
    args: &'a [String],
) -> Result<(&'a str, &'a str), ExitCode> {
    if args.len() < 2 {
        eprintln!("mock-fs {cmd}: usage: <image> <path>");
        return Err(ExitCode::from(2));
    }
    Ok((args[0].as_str(), args[1].as_str()))
}

fn cmd_ls(args: &[String]) -> ExitCode {
    let (_image, path) = match parse_image_and_path("ls", args) {
        Ok(t) => t,
        Err(c) => return c,
    };
    if path == "/" || path.is_empty() {
        println!("hello.txt");
        ExitCode::SUCCESS
    } else if path == "/hello.txt" {
        // A leaf path -- empty listing.
        ExitCode::SUCCESS
    } else {
        eprintln!("mock-fs ls: not found: {path}");
        ExitCode::from(1)
    }
}

fn cmd_cat(args: &[String]) -> ExitCode {
    let (_image, path) = match parse_image_and_path("cat", args) {
        Ok(t) => t,
        Err(c) => return c,
    };
    if path == "/hello.txt" {
        // Bytes, no platform-specific line ending fixup.
        let _ = io::stdout().write_all(HELLO_BODY.as_bytes());
        let _ = io::stdout().flush();
        ExitCode::SUCCESS
    } else {
        eprintln!("mock-fs cat: not found: {path}");
        ExitCode::from(1)
    }
}

fn cmd_stat(args: &[String]) -> ExitCode {
    let (_image, path) = match parse_image_and_path("stat", args) {
        Ok(t) => t,
        Err(c) => return c,
    };
    if path == "/hello.txt" {
        println!("path={path}");
        println!("type=file");
        println!("size={}", HELLO_BODY.len());
        println!("mode=0644");
        ExitCode::SUCCESS
    } else {
        eprintln!("mock-fs stat: not found: {path}");
        ExitCode::from(1)
    }
}
