#!/usr/bin/env bash
# _lib_harness.sh -- shared helpers for the harness scripts.
#
# Sourced by setup-local.sh, test-windows-matrix.sh, etc. Defines:
#   harness_root            absolute path to this fs-test-harness checkout
#   consumer_root           absolute path to the consumer repo (cwd by default)
#   harness_toml            path to the consumer's harness.toml
#   harness_get KEY         echoes the dotted-path value from harness.toml
#   harness_get_or KEY DEF  same, with default
#
# Reads harness.toml via python3. We don't require Python's `tomllib`
# (3.11+); we do a minimal hand parse that handles the limited subset
# we actually use ([section], key = "value", arrays of strings, ints,
# bools). For richer needs install Python 3.11 or `tomli`.

# shellcheck disable=SC2034   # variables are consumed by callers
harness_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
consumer_root="${CONSUMER_ROOT:-${PWD}}"
harness_toml="${HARNESS_TOML:-${consumer_root}/harness.toml}"

# harness_get <dotted.path>
# Echoes the value (string / int / bool / json-array) at the given
# dotted path in $harness_toml. Exit 0 on hit, exit 1 if absent.
harness_get() {
    local key="$1"
    if [[ ! -f "${harness_toml}" ]]; then
        return 1
    fi
    python3 - "${harness_toml}" "${key}" <<'PY'
import json, re, sys
path = sys.argv[1]; key = sys.argv[2]
try:
    import tomllib
    with open(path, 'rb') as f:
        data = tomllib.load(f)
except Exception:
    try:
        import tomli as tomllib   # noqa
        with open(path, 'rb') as f:
            data = tomllib.load(f)
    except Exception:
        # Hand parse: section + key = value (subset)
        data = {}
        section = data
        with open(path) as f:
            for line in f:
                s = line.strip()
                if not s or s.startswith('#'): continue
                m = re.match(r'^\[([^\]]+)\]\s*$', s)
                if m:
                    section = data
                    for part in m.group(1).split('.'):
                        section = section.setdefault(part, {})
                    continue
                m = re.match(r'^([\w\-]+)\s*=\s*(.*?)\s*(?:#.*)?$', s)
                if not m: continue
                k, v = m.group(1), m.group(2)
                if v.startswith('"') and v.endswith('"'):
                    section[k] = v[1:-1]
                elif v in ('true', 'false'):
                    section[k] = (v == 'true')
                elif v.startswith('['):
                    # naive array of strings
                    inner = v.strip('[]').strip()
                    if not inner:
                        section[k] = []
                    else:
                        section[k] = [
                            x.strip().strip('"') for x in inner.split(',') if x.strip()
                        ]
                else:
                    try:    section[k] = int(v)
                    except: section[k] = v
node = data
for part in key.split('.'):
    if isinstance(node, dict) and part in node:
        node = node[part]
    else:
        sys.exit(1)
if isinstance(node, (list, dict)):
    sys.stdout.write(json.dumps(node))
elif isinstance(node, bool):
    sys.stdout.write("true" if node else "false")
else:
    sys.stdout.write(str(node))
PY
}

harness_get_or() {
    local key="$1" default="${2:-}"
    local v
    if v=$(harness_get "${key}" 2>/dev/null) && [[ -n "${v}" ]]; then
        printf '%s' "${v}"
    else
        printf '%s' "${default}"
    fi
}
