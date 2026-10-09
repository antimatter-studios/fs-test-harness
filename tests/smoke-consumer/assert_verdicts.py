#!/usr/bin/env python3
"""assert_verdicts.py -- check what the harness reported for one smoke phase.

    assert_verdicts.py pass|canary <exit-code> <matrix> <prefix> <diag-dir> <log>

Reads only the harness's own outputs -- its exit code, the diagnostics
tree run-matrix writes (run-manifest.json, results.json and, per
scenario, result.json, recipe.json and step-NN/{step.json,stdout.txt,
stderr.txt}) and its log -- and fails loudly on anything that does not
match the matrix it was given.
"""

import json
import os
import re
import sys

# What each built-in VM op script prints on success. Proves the op
# really ran against the mounted drive, not just that something exited 0.
VM_OP_STDOUT = {
    "win-ls-via-mount": r"^ok [A-Z]:\\.* \(\d+ entries\)",
    "win-cat-via-mount": r"^ok [A-Z]:\\.* \(\d+ bytes\)",
    "win-write": r"^wrote \d+ bytes to [A-Z]:\\",
    "win-mkdir": r"^mkdir [A-Z]:\\",
    "win-rename": r"^renamed [A-Z]:\\.* -> [A-Z]:\\",
    "win-unlink": r"^removed [A-Z]:\\",
    "win-rmdir": r"^removed dir [A-Z]:\\",
}

failures = []


def check(cond, msg):
    if not cond:
        failures.append(msg)
    return cond


def load(path):
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def read(path):
    try:
        with open(path, encoding="utf-8", errors="replace") as f:
            return f.read()
    except FileNotFoundError:
        return None


def check_common(diag, scenarios, expect_status):
    manifest = os.path.join(diag, "run-manifest.json")
    if check(os.path.isfile(manifest), f"missing {manifest}"):
        m = load(manifest)
        check(m.get("scenario_count_runnable") == len(scenarios),
              f"run-manifest.json scenario_count_runnable={m.get('scenario_count_runnable')}"
              f", want {len(scenarios)}")

    results_path = os.path.join(diag, "results.json")
    if check(os.path.isfile(results_path), f"missing {results_path}"):
        results = {r["name"]: r["status"] for r in load(results_path)}
        want = {name: expect_status for name in scenarios}
        check(results == want, f"results.json verdicts {results}, want {want}")


def step_dirs_complete(sdir):
    for f in ("step.json", "stdout.txt", "stderr.txt"):
        check(os.path.isfile(os.path.join(sdir, f)), f"missing {sdir}/{f}")


def check_pass(rc, scenarios, diag, log):
    check(rc == 0, f"run-tests.sh exited {rc}, want 0")
    check("retrying" not in log,
          "the runner retried a scenario: smoke scenarios must pass first time")
    check_common(diag, scenarios, "passed")

    for name, scn in scenarios.items():
        sdir = os.path.join(diag, name)
        result = load(os.path.join(sdir, "result.json"))
        check(result["status"] == "passed" and result["error"] is None,
              f"{name}: result.json {result}")
        recipe = load(os.path.join(sdir, "recipe.json"))
        steps = recipe["steps"]
        check(recipe["overall_passed"] is True, f"{name}: recipe.json overall_passed is false")
        check(len(steps) == len(scn["recipe"]),
              f"{name}: {len(steps)} steps executed, recipe has {len(scn['recipe'])}")
        for step in steps:
            label = f"{name} step {step['index']:02d} ({step['op']})"
            check(not step["skipped"], f"{label}: skipped")
            check(step["error"] is None and step["exit_code"] == step["expected_exit"],
                  f"{label}: exit {step['exit_code']} error {step['error']}")
            step_dir = os.path.join(sdir, f"step-{step['index']:02d}")
            step_dirs_complete(step_dir)
            pattern = VM_OP_STDOUT.get(step["op"])
            if pattern:
                check(step["host"] == "vm", f"{label}: ran on {step['host']}, want vm")
                stdout = read(os.path.join(step_dir, "stdout.txt")) or ""
                check(re.search(pattern, stdout, re.M),
                      f"{label}: stdout {stdout.strip()!r} does not match {pattern!r}")
                literal_path = scn["recipe"][step["index"]].get("literal_path")
                if literal_path:
                    check(literal_path.replace("/", "\\") in stdout,
                          f"{label}: Windows output lost the literal path {literal_path!r}")

        # The runner deletes each scenario's staged host image when the
        # scenario ends; the newest run dir must hold no image.
        images = os.path.join(os.path.dirname(diag), "..", "images")
        runs = sorted((d for d in os.listdir(images) if d.isdigit()), key=int) if os.path.isdir(images) else []
        if check(runs, f"no run dir under {images}"):
            left = os.path.join(images, runs[-1], scn["image"])
            check(not os.path.exists(left), f"{name}: staged image not cleaned up: {left}")


def check_canary(rc, scenarios, diag, log):
    check(rc != 0, "run-tests.sh exited 0 although canary scenarios must fail")
    check("retrying" not in log,
          "the runner retried the canary: a first-attempt failure must stay red")
    check(re.search(r"failed on first attempt", log),
          "log does not report the canary as failing on its first attempt")
    check_common(diag, scenarios, "failed")

    for name, scn in scenarios.items():
        sdir = os.path.join(diag, name)
        result = load(os.path.join(sdir, "result.json"))
        check(result["status"] == "failed", f"{name}: status {result['status']!r}, want 'failed'")
        last = len(scn["recipe"]) - 1
        check(f"step {last} (win-cat-via-mount) exit Some(1) != expected 0" in (result["error"] or ""),
              f"{name}: error does not name the failing step: {result['error']!r}")
        check("content mismatch" in (result["error"] or ""),
              f"{name}: error does not carry the verifier's reason: {result['error']!r}")
        recipe = load(os.path.join(sdir, "recipe.json"))
        steps = recipe["steps"]
        check(recipe["overall_passed"] is False, f"{name}: recipe.json overall_passed is true")
        check(len(steps) == len(scn["recipe"]),
              f"{name}: {len(steps)} steps recorded, want {len(scn['recipe'])}")
        for step in steps[:-1]:
            check(step["exit_code"] == step["expected_exit"],
                  f"{name} step {step['index']} ({step['op']}) failed before the canary step")
            step_dirs_complete(os.path.join(sdir, f"step-{step['index']:02d}"))
        step_dir = os.path.join(sdir, f"step-{last:02d}")
        step_dirs_complete(step_dir)
        stderr = read(os.path.join(step_dir, "stderr.txt")) or ""
        check("content mismatch" in stderr, f"{name}: step stderr lacks the mismatch: {stderr!r}")


def main(argv):
    if len(argv) != 7 or argv[1] not in ("pass", "canary"):
        raise SystemExit(__doc__)
    mode, rc, matrix, prefix, diag, log_path = argv[1:]
    scenarios = {n: s for n, s in load(matrix)["scenarios"].items() if n.startswith(prefix)}
    if not scenarios:
        raise SystemExit(f"no scenarios in {matrix} start with {prefix!r}")
    log = read(log_path) or ""
    check("#< CLIXML" not in log,
          "PowerShell CLIXML leaked into the smoke transcript")
    (check_pass if mode == "pass" else check_canary)(int(rc), scenarios, diag, log)

    print(f"--- {mode} verdicts ({prefix}*) ---")
    for name in scenarios:
        result = read(os.path.join(diag, name, "result.json"))
        status = json.loads(result)["status"] if result else "<no result.json>"
        print(f"  {name}: {status}")
    if failures:
        print(f"FAIL: {len(failures)} assertion(s):", file=sys.stderr)
        for f in failures:
            print(f"  - {f}", file=sys.stderr)
        return 1
    print(f"OK: {mode} assertions hold")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
