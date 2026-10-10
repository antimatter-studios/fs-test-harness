#!/usr/bin/env python3
"""Run QEMU with local allocation instead of inherited Linux interleaving.

Only this launcher and its exec'd QEMU change policy. MPOL_LOCAL asks the
kernel to choose the local node and permits fallback; no node is hardcoded.
Explicit bind/preferred policies and the system default remain unchanged.
"""

import ctypes
import errno
import os
import platform
import sys

MPOL_INTERLEAVE = 3
MPOL_LOCAL = 4


class MemoryAPI:
    """Linux ARM64's kernel ABI; libc supplies syscall, with no new library."""

    def __init__(self):
        if platform.machine().lower() not in ("aarch64", "arm64"):
            raise RuntimeError("Linux memory launcher requires a supported ARM64 host")
        self.libc = ctypes.CDLL(None, use_errno=True)
        self.libc.syscall.restype = ctypes.c_long

    def get_mempolicy(self, mode, mask, size, address, flags):
        return self.libc.syscall(236, mode, mask, ctypes.c_ulong(size), address, flags)

    def set_mempolicy(self, mode, mask, size):
        return self.libc.syscall(237, mode, mask, ctypes.c_ulong(size))


def read_policy(lib):
    mode = ctypes.c_int()
    if lib.get_mempolicy(ctypes.byref(mode), None, 0, None, 0) != 0:
        if ctypes.get_errno() == errno.ENOSYS:
            return None
        raise RuntimeError(f"get_mempolicy failed: {os.strerror(ctypes.get_errno())}")
    return mode.value


def normalize_policy(lib):
    inherited = read_policy(lib)
    if inherited is None:
        return "Linux memory policy: kernel has no NUMA policy API; native allocation retained"
    # The two flags qualify the nodemask, not the policy itself.
    policy = inherited & ~((1 << 14) | (1 << 15))
    if policy != MPOL_INTERLEAVE:
        return f"Linux memory policy: inherited mode {inherited} retained"
    if lib.set_mempolicy(MPOL_LOCAL, None, 0) != 0:
        raise RuntimeError(f"set_mempolicy failed: {os.strerror(ctypes.get_errno())}")
    if read_policy(lib) == MPOL_LOCAL:
        return "Linux memory policy: inherited interleave changed to local allocation"
    return (
        "Linux memory policy: kernel retained inherited interleave despite local request; "
        "QEMU will use the unchanged policy (check host kernel configuration)"
    )


def main(command):
    if not command:
        raise RuntimeError("Linux memory launcher needs a QEMU command")
    print(normalize_policy(MemoryAPI()), flush=True)
    os.execvp(command[0], command)


if __name__ == "__main__":
    try:
        main(sys.argv[1:])
    except (OSError, RuntimeError) as exc:
        print(f"QEMU memory policy failed: {exc}", file=sys.stderr)
        sys.exit(1)
