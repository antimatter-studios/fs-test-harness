# Consumer integration

How a new project plugs into `fs-windows-test-harness`. Read this end-to-end the
first time; you can skim it on subsequent project setups.

## What you need before starting

- A Rust filesystem driver (or formatter) project with a CLI binary.
- A Windows VM reachable over SSH. [`vm-setup.md`](./vm-setup.md) sets one up,
  including the fixed host-only address `VM_HOST` should name. Step 4
  provisions its Rust toolchain and packages.
- The driver's binary either prebuilt and synced onto the VM, or
  buildable via `cargo build --release` on the VM (the harness can do
  the sync + build for you, but does not require it).

## 1. Pin the harness as a sibling

Add a release tag to your project's `chores.yml`:

```yaml
vars:
  HARNESS_REF: vX.Y.Z
```

Add `fs-windows-test-harness` to that file's `siblings` task, with the URL
`https://github.com/antimatter-studios/fs-windows-test-harness.git`, ref
`{{.HARNESS_REF}}`, and checkout path `../fs-windows-test-harness`. The task
should refuse to move a dirty checkout. The
[`rust-fs-ntfs` sibling task](https://github.com/antimatter-studios/rust-fs-ntfs/blob/main/chores.yml)
shows a complete implementation. Run `chore siblings` from your project
before using the harness. Its scripts and schemas now live in the sibling
directory.

Do not add a git submodule: it would put a separate harness copy inside each
consumer, outside the ref that `chore siblings` maintains.

## 2. Write `fs-windows-test-harness.toml`

Create `fs-windows-test-harness.toml` at the root of your project. Minimum:

```toml
[project]
name   = "my-driver"
binary = "target/release/my-driver.exe"

[vm]
host    = "you@192.168.1.123"
ssh_key = "~/.ssh/id_ed25519"
workdir = "C:/Users/you/dev/my-driver-work"

[ops]
ls   = "{binary} ls {image} {path}"
cat  = "{binary} cat {image} {path}"
stat = "{binary} stat {image} {path}"
```

See [`../schemas/harness.schema.json`](../schemas/harness.schema.json)
for the full surface, and
[`../examples/minimal/fs-windows-test-harness.toml`](../examples/minimal/fs-windows-test-harness.toml)
for an annotated minimal example.

### Reserved substitution tokens

Used in `[ops]` and `[post_verify]` templates:

| Token | Source | Notes |
|---|---|---|
| `{binary}` | `[project] binary` | Path on the VM. |
| `{image}` | scenario `image` | Resolved against `[vm] image_dir`. |
| `{drive}` | runtime | Free Windows drive letter, picked just before mount. |
| `{path}`, `{from}`, `{to}` | per-op | From the op's matching field. |
| `{content}` | per-op | UTF-8; for binary use `content_b64`. |
| `{extra}` | recipe step | Extra argument supplied by that step. |
| `{tools.<name>}` | `[tools]` table | E.g. `{tools.fsck}` resolves to `[tools] fsck`. |

## 3. Write `test-matrix.json`

Create `test-matrix.json` at the project root. Schema in
[`../schemas/test-matrix.schema.json`](../schemas/test-matrix.schema.json).
A first scenario:

```json
{
  "scenarios": {
    "ro-list-root": {
      "status": "pending",
      "image": "fixtures/sample.img",
      "recipe": [
        {
          "host": "vm",
          "op": "ls",
          "path": "/",
          "expect_names": [".", "..", "lost+found"]
        }
      ]
    }
  }
}
```

`expect_*` fields are pass/fail criteria. The runner compares each op's
output against the declared expectations; mismatch is a failed verdict.
Capture real values once with the driver running locally, paste them in.

When several scenarios compete for one scarce resource, give them the same
non-empty `exclusive_group`. Those scenarios execute one at a time while all
other scenarios retain the parallelism configured by
`[runner].max_parallel`. A failed scenario is never retried automatically:
the first-attempt diagnostics and non-zero verdict are preserved.

## 4. VM package provisioning

The Windows-side `scripts/setup-windows-vm.ps1` installs the declared Rust
toolchain and any winget packages in `[vm.packages]` of
`fs-windows-test-harness.toml`. To have the harness copy and invoke that
script on the VM, run this from your project:

```sh
bash ../fs-windows-test-harness/scripts/run-tests.sh --reinstall ro-list-root
```

This removes and reinstalls the listed packages before running the scenario;
use it when first provisioning them or changing their installer features.

`[vm.packages]` entries are either bare strings (installed with the
package's default feature set) or tables for packages that need
non-default installer features:

```toml
[vm]
packages = [
    "MartinStorsjo.LLVM-MinGW.UCRT",   # bare string — default features
    "LLVM.LLVM",
    { id = "WinFsp.WinFsp", custom_args = "ADDLOCAL=F.Main,F.User,F.Developer" },
]
```

`custom_args` is forwarded to the underlying installer via winget's
`--override`. The WinFsp example pulls in `F.Main` + `F.User` (the
default runtime) + `F.Developer` (headers + `.lib`) — required for
consumers that build WinFsp bindings via `bindgen` (`ext4-win-driver`,
`erofs-win-driver`). The `--reinstall` flow passes these values to
`setup-windows-vm.ps1` as `-PackagesJson`. A package installed without the
required features needs an uninstall and reinstall; winget reconfigure
with a new `ADDLOCAL` set can return 1603.

(Mac-side `.test-env` is bootstrapped automatically by `run-tests.sh`
on first run — see step 5.)

## 5. Run a scenario

```sh
bash ../fs-windows-test-harness/scripts/run-tests.sh ro-list-root
```

On the very first run, prompts for VM host / ssh key / workdir and
writes `.test-env` (gitignored). On every run: tars the project source
+ fixtures, ssh's to the VM, runs `run-matrix --filter ro-list-root`
over there, retrieves the diag tree to `test-diagnostics/run-<UTC>/`.
The first run takes longer due to cargo build on the VM; subsequent
runs reuse the build cache.

```sh
bash ../fs-windows-test-harness/scripts/run-tests.sh           # whole matrix
bash ../fs-windows-test-harness/scripts/run-tests.sh --list    # list scenarios
bash ../fs-windows-test-harness/scripts/run-tests.sh --reset   # wipe .test-env, re-prompt
bash ../fs-windows-test-harness/scripts/run-tests.sh --help    # full flag surface
```

## 6. Read the diag

```
test-diagnostics/run-2026-05-07T19-23-04Z/
  run-manifest.json                 # project, host, sha, scenario count
  results.json                      # one row per scenario: name + verdict
  ro-list-root/
    manifest.json                   # verdict + op trace summary
    scenario.json                   # what the PS runner saw
    mount-stdout.txt
    mount-stderr.txt
    op-trace.jsonl                  # one line per op: input, output, verdict
    op00-stdout.txt                 # per-op stdout
    op00-stderr.txt
    ps-stdout.txt                   # full PowerShell transcript
    ps-stderr.txt
    result.json
```

Start at `manifest.json` for the verdict. If `failed`, walk
`op-trace.jsonl` to find the offending op, then open the matching
`opNN-stdout.txt` / `opNN-stderr.txt`. See
[`triage-protocol.md`](triage-protocol.md) for the full checklist.

## 7. Add more scenarios

Iterate. Capture expected values by running the driver locally and
copy-pasting outputs. Multiple agents on the same project can claim
scenarios in parallel — see [`multi-agent-protocol.md`](multi-agent-protocol.md).

## Migrating existing projects

If your project already has its own copy of these scripts (e.g. you
forked from `rust-fs-ntfs` or the early `ext4-win-driver`), the
migration is mostly mechanical:

1. Move project-specific scenarios into `test-matrix.json` (likely
   already there).
2. Translate hardcoded shell commands into `[ops]` templates in
   `fs-windows-test-harness.toml`.
3. Replace obsolete copies of the harness scripts and `tests/matrix.rs`;
   keep project-specific scripts used by your scenarios.
4. Pin `HARNESS_REF` and add the sibling checkout to `chore siblings` as in
   step 1.
5. Run one scenario to verify the new wiring before deleting old
   tooling.

## Common gotchas

- **`expect_*` values that drift on every run** (timestamps, FUSE
  inode numbers, atimes) — quote them with care or use
  `expect_stdout_contains` for partial matches.
- **Paths in `fs-windows-test-harness.toml`** are resolved relative to that
  file, not the repository root. Stay consistent.
- **PowerShell 5.1 quirks** on Windows VMs: avoid char ranges (`'A'..'Z'`)
  and pwsh-only operators in any custom op templates.
- **WinFsp drive letters are per-logon-session** — if you SSH into the
  VM and mount, the desktop session does not see the drive. Run mounts
  from the desktop console, or use a directory mount path.
