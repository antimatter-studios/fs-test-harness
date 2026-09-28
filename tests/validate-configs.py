#!/usr/bin/env python3
"""validate-configs.py -- consumer config checks, positive and negative.

Every consumer directory in this repo (a directory holding a
fs-windows-test-harness.toml: examples/*, tests/smoke-consumer) must:

  * parse, and validate against schemas/harness.schema.json;
  * point at a matrix that validates against schemas/test-matrix.schema.json;
  * only use recipe ops that its [ops] declares (or the built-in
    ship-to-vm / ship-to-host) -- the runner only discovers an undeclared
    op mid-run, after executing every step before it.

Every fixture under tests/config-fixtures/invalid*/ must be REJECTED by
those same checks, with an error containing the text in its expect.txt,
so a loosened schema or a check that stopped firing fails here too.
(runner/src/tests.rs asserts the runner's loader rejects the
tests/config-fixtures/invalid/ set as well; invalid-references/ holds
the cases only this cross-check catches.)

Needs Python 3.11+ (tomllib) and the `jsonschema` package.
"""

import json
import pathlib
import sys
import tomllib

from jsonschema import Draft202012Validator

ROOT = pathlib.Path(__file__).resolve().parent.parent
CONFIG = "fs-windows-test-harness.toml"
BUILTIN_OPS = {"ship-to-vm", "ship-to-host"}


def schema(name):
    s = json.loads((ROOT / "schemas" / name).read_text())
    Draft202012Validator.check_schema(s)
    return Draft202012Validator(s)


HARNESS = schema("harness.schema.json")
MATRIX = schema("test-matrix.schema.json")


def schema_errors(validator, doc, label):
    out = []
    pending = list(validator.iter_errors(doc))
    while pending:
        e = pending.pop(0)
        where = "/".join(map(str, e.absolute_path)) or "<root>"
        # Keyword and instance path are schema facts; jsonschema's English
        # message changed between 4.19 and later releases for minLength.
        out.append(f"{label} {where} [{e.validator}]: {e.message}")
        # oneOf / anyOf failures carry the per-branch reasons here.
        pending.extend(e.context)
    return out


def consumer_errors(directory):
    """All problems with one consumer directory; [] means valid."""
    errors = []
    try:
        cfg = tomllib.loads((directory / CONFIG).read_text())
    except tomllib.TOMLDecodeError as e:
        return [f"{CONFIG}: {e}"]
    errors += schema_errors(HARNESS, cfg, CONFIG)

    project = cfg.get("project") if isinstance(cfg.get("project"), dict) else {}
    matrix_name = project.get("matrix_path", "test-matrix.json")
    try:
        matrix = json.loads((directory / matrix_name).read_text())
    except (OSError, json.JSONDecodeError) as e:
        return errors + [f"{matrix_name}: {e}"]
    errors += schema_errors(MATRIX, matrix, matrix_name)

    declared = set(cfg.get("ops", {})) | BUILTIN_OPS
    scenarios = matrix.get("scenarios", {}) if isinstance(matrix, dict) else {}
    for name, scn in scenarios.items():
        recipe = scn.get("recipe", []) if isinstance(scn, dict) else []
        for i, step in enumerate(recipe if isinstance(recipe, list) else []):
            op = step.get("op") if isinstance(step, dict) else None
            if isinstance(op, str) and op not in declared:
                errors.append(
                    f"{matrix_name} scenarios/{name}/recipe/{i}: "
                    f"op '{op}' is not declared in {CONFIG} [ops]"
                )
    return errors


def main():
    fixtures = ROOT / "tests" / "config-fixtures"
    consumers = sorted(
        p.parent for p in ROOT.rglob(CONFIG)
        if fixtures not in p.parents and "target" not in p.parts
    )
    negatives = sorted(p.parent for p in fixtures.glob(f"invalid*/*/{CONFIG}"))
    failed = False

    for d in consumers:
        rel = d.relative_to(ROOT)
        errors = consumer_errors(d)
        if errors:
            failed = True
            print(f"FAIL {rel}: valid consumer config rejected")
            for e in errors:
                print(f"       {e}")
        else:
            print(f"OK   {rel}: valid")

    for d in negatives:
        rel = d.relative_to(ROOT)
        expect = (d / "expect.txt").read_text().strip()
        errors = consumer_errors(d)
        if not errors:
            failed = True
            print(f"FAIL {rel}: invalid config was accepted")
        elif not any(expect in e for e in errors):
            failed = True
            print(f"FAIL {rel}: rejected, but not for {expect!r}:")
            for e in errors:
                print(f"       {e}")
        else:
            print(f"OK   {rel}: rejected ({expect})")

    if not consumers or not negatives:
        print("FAIL: found no consumer configs or no negative fixtures")
        failed = True
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
