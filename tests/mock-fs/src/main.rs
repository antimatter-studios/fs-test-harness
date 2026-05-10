//! mock-fs -- stand-in CLI for fs-test-harness's mock-scenario CI job.
//!
//! Pretends to be a filesystem driver so the harness's full loop can be
//! exercised end-to-end without needing WinFsp, a real disk image, or
//! an SSH'd VM. Subcommands:
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
        eprintln!("usage: mock-fs <ls|cat|stat> ...");
        return ExitCode::from(2);
    }
    match args[0].as_str() {
        "ls" => cmd_ls(&args[1..]),
        "cat" => cmd_cat(&args[1..]),
        "stat" => cmd_stat(&args[1..]),
        other => {
            eprintln!("mock-fs: unknown subcommand: {other}");
            ExitCode::from(2)
        }
    }
}

fn parse_image_and_path<'a>(cmd: &str, args: &'a [String]) -> Result<(&'a str, &'a str), ExitCode> {
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
