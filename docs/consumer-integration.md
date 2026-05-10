# Consumer integration

How a new project plugs into `fs-test-harness`. Read this end-to-end the
first time; you can skim it on subsequent project setups.

## What you need before starting

- A Rust filesystem driver (or formatter) project with a CLI binary.
- A Windows VM reachable over SSH, with rustup installed and the
  toolchain your driver builds with.
- The driver's binary either prebuilt and synced onto the VM, or
  buildable via `cargo build --release` on the VM (the harness can do
  the sync + build for you, but does not require it).

## 1. Add the harness to your project

Until we tag a release and publish to crates.io, vendor by path or
submodule:

```sh
cd your-project
git submodule add ../fs-test-harness harness
```

(Or `git clone` it as a sibling and reference via path-dep — the
runner's Cargo manifest is permissive about being included as a path
dep from a parent project.)

You should now have a `harness/` directory in your project containing
the scripts, runner crate, schemas, and docs.

## 2. Write `harness.toml`

Create `harness.toml` at the *root of your project* (not inside
`harness/`). Minimum:

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

[mount]
command    = "{binary} mount {image} --drive {drive} {extra}"
ready_line = "mounted at"
rw_extra   = "--rw"
```

See [`../schemas/harness.schema.json`](../schemas/harness.schema.json)
for the full surface, and `examples/minimal/harness.toml` for an
annotated minimal example.

### Reserved substitution tokens

Used in `[ops]`, `[mount]`, and `[post_verify]` templates:

| Token | Source | Notes |
|---|---|---|
| `{binary}` | `[project] binary` | Path on the VM. |
| `{image}` | scenario `image` | Resolved against `[vm] image_dir`. |
| `{drive}` | runtime | Free Windows drive letter, picked just before mount. |
| `{path}`, `{from}`, `{to}` | per-op | From the op's matching field. |
| `{content}` | per-op | UTF-8; for binary use `content_b64`. |
| `{extra}` | mount | Default `[mount] default_extra`, or `rw_extra` when scenario requests RW. |
| `{tools.<name>}` | `[tools]` table | E.g. `{tools.fsck}` resolves to `[tools] fsck`. |

## 3. Write `test-matrix.json`

Create `test-matrix.json` at the project root. Schema in
[`../schemas/test-matrix.schema.json`](../schemas/test-matrix.schema.json).
A first scenario:

```json
{
  "_format": "v1",
  "scenarios": {
    "ro-list-root": {
      "status": "pending",
      "image": "fixtures/sample.img",
      "ops": [
        {
          "type": "ls",
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

## 4. One-time VM provisioning

```sh
bash harness/scripts/setup-windows-vm.ps1   # run on the VM, once
```

Installs rustup + the toolchain you declared, and any winget packages
listed in `harness.toml [vm.packages]`. Idempotent.

(Mac-side `.test-env` is bootstrapped automatically by `run-tests.sh`
on first run — see step 5.)

## 5. Run a scenario

```sh
bash harness/scripts/run-tests.sh ro-list-root
```

On the very first run, prompts for VM host / ssh key / workdir and
writes `.test-env` (gitignored). On every run: tars the project source
+ fixtures, ssh's to the VM, runs `run-matrix --filter ro-list-root`
over there, retrieves the diag tree to `test-diagnostics/run-<UTC>/`.
The first run takes longer due to cargo build on the VM; subsequent
runs reuse the build cache.

```sh
bash harness/scripts/run-tests.sh           # whole matrix
bash harness/scripts/run-tests.sh --list    # list scenarios
bash harness/scripts/run-tests.sh --reset   # wipe .test-env, re-prompt
bash harness/scripts/run-tests.sh --help    # full flag surface
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
2. Translate hardcoded shell commands into `harness.toml [ops]`
   templates.
3. Delete the old `scripts/` and `tests/matrix.rs` (the harness
   replaces both).
4. Add a `harness/` submodule pointing at this repo.
5. Run one scenario to verify the new wiring before deleting old
   tooling.

## Common gotchas

- **`expect_*` values that drift on every run** (timestamps, FUSE
  inode numbers, atimes) — quote them with care or use
  `expect_stdout_contains` for partial matches.
- **Paths in `harness.toml`** are resolved relative to the toml file,
  not the repo root. Stay consistent.
- **PowerShell 5.1 quirks** on Windows VMs: avoid char ranges (`'A'..'Z'`)
  and pwsh-only operators in any custom op templates.
- **WinFsp drive letters are per-logon-session** — if you SSH into the
  VM and mount, the desktop session does not see the drive. Run mounts
  from the desktop console, or use a directory mount path.
