#!/usr/bin/env python3
"""tarimg.py -- the smoke consumer's host-side "filesystem binary".

The smoke consumer's volumes are tar archives: the host creates one,
the harness ships it to the Windows host, memfs-mount.ps1 mounts it on
a drive letter through WinFsp's memfs, and the harness ships it back.
This CLI is the host half of that driver -- the part a real consumer's
binary plays when it formats an image or reads one back for the
harness's scripts/host/verify-*.sh verifiers:

    tarimg.py create <image> [<path>=<content> ...]
    tarimg.py ls     <image> <dir>     one entry name per line
    tarimg.py cat    <image> <file>    raw bytes, nothing added
    tarimg.py info   <image>           label (the image's file name, in
                                       double quotes) and file count

Paths inside the image are volume-relative ("/docs/a.txt").
Standard library only, so it runs wherever the harness's python3 does.
"""

import io
import os
import sys
import tarfile
import time


def norm(path):
    """Volume path -> archive member name ("" is the root)."""
    return "/".join(p for p in path.replace("\\", "/").split("/") if p not in ("", "."))


def create(image, *files):
    os.makedirs(os.path.dirname(os.path.abspath(image)), exist_ok=True)
    with tarfile.open(image, "w", format=tarfile.USTAR_FORMAT) as tar:
        for spec in files:
            path, sep, content = spec.partition("=")
            if not sep:
                raise SystemExit(f"create: expected <path>=<content>, got {spec!r}")
            data = content.encode("utf-8")
            info = tarfile.TarInfo(norm(path))
            info.size = len(data)
            info.mode = 0o644
            info.mtime = int(time.time())
            tar.addfile(info, io.BytesIO(data))
    print(f"created {image} ({len(files)} file(s))")


def ls(image, path):
    want = norm(path)
    prefix = want + "/" if want else ""
    names = set()
    with tarfile.open(image, "r") as tar:
        entries = [norm(m.name) for m in tar.getmembers()]
    if want and not any(e == want or e.startswith(prefix) for e in entries):
        raise SystemExit(f"ls: {path}: no such directory in {image}")
    for e in entries:
        if e and e.startswith(prefix) and e != want:
            names.add(e[len(prefix) :].split("/", 1)[0])
    # Bytes with "\n", not print(): on Windows text-mode stdout writes
    # "\r\n", and the harness's bash verifiers would read "docs\r".
    sys.stdout.buffer.write("".join(n + "\n" for n in sorted(names)).encode("utf-8"))
    sys.stdout.buffer.flush()


def cat(image, path):
    want = norm(path)
    with tarfile.open(image, "r") as tar:
        for m in tar.getmembers():
            if norm(m.name) == want and m.isfile():
                sys.stdout.buffer.write(tar.extractfile(m).read())
                sys.stdout.buffer.flush()
                return
    raise SystemExit(f"cat: {path}: no such file in {image}")


def info(image):
    # The label is printed in double quotes, so a verifier's needle carries
    # them too: that is what proves a host-side op's arguments reach it with
    # their quotes intact on every orchestrator.
    label = os.path.splitext(os.path.basename(image))[0]
    with tarfile.open(image, "r") as tar:
        files = sum(1 for m in tar.getmembers() if m.isfile())
    out = f'label:          "{label}"\nfiles:          {files}\n'
    sys.stdout.buffer.write(out.encode("utf-8"))
    sys.stdout.buffer.flush()


def main(argv):
    commands = {
        "create": (create, 1, None),
        "ls": (ls, 2, 2),
        "cat": (cat, 2, 2),
        "info": (info, 1, 1),
    }
    if len(argv) < 2 or argv[1] not in commands:
        raise SystemExit("usage: tarimg.py create|ls|cat|info <image> ...")
    fn, lo, hi = commands[argv[1]]
    args = argv[2:]
    if len(args) < lo or (hi is not None and len(args) > hi):
        raise SystemExit(f"{argv[1]}: wrong number of arguments")
    fn(*args)


if __name__ == "__main__":
    main(sys.argv)
