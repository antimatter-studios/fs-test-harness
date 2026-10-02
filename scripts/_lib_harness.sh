#!/usr/bin/env bash
# _lib_harness.sh -- shared helpers for the harness scripts.
#
# Sourced by run-tests.sh and the state-machine helpers. Defines:
#   harness_root            absolute path to this fs-windows-test-harness checkout
#   consumer_root           absolute path to the consumer repo (cwd by default)
#   harness_toml            path to the consumer's fs-windows-test-harness.toml
#   harness_get KEY         echoes the dotted-path value from harness.toml
#   harness_get_or KEY DEF  same, with default
#   harness_bounded S L CMD runs CMD for at most S seconds; a timeout names L
#   harness_timeout_seconds  reads and checks one of the FSWTH_*_TIMEOUT knobs
#
# Reads harness.toml via python3. We don't require Python's `tomllib`
# (3.11+); we do a minimal hand parse that handles the limited subset
# we actually use ([section], key = "value", arrays of strings, ints,
# bools). For richer needs install Python 3.11 or `tomli`.

# shellcheck disable=SC2034   # variables are consumed by callers
harness_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# Both paths are made ABSOLUTE here, against the directory the script was
# invoked from. run-tests.sh later cd's into consumer_root and exports
# HARNESS_TOML for the Rust runner, so a relative path would be read from one
# directory by the shell and from another by the runner -- a different config,
# or none. A Windows drive path (C:\... or C:/...) is already absolute.
harness_abs_path() {
    case "$1" in
        /* | [A-Za-z]:[\\/]*) printf '%s' "$1" ;;
        *) printf '%s/%s' "${PWD}" "$1" ;;
    esac
}
consumer_root="$(harness_abs_path "${CONSUMER_ROOT:-${PWD}}")"
# Consumer config: HARNESS_TOML wins; otherwise fs-windows-test-harness.toml
# in the consumer root (runner/src/bin/run-matrix.rs resolves the same way).
harness_toml="$(harness_abs_path "${HARNESS_TOML:-${consumer_root}/fs-windows-test-harness.toml}")"

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

# harness_timeout_seconds NAME DEFAULT
# Echoes the value of environment variable NAME, or DEFAULT when it is unset,
# and fails loudly when the value is not a whole number of seconds in
# 1..86400. A bound nobody can read back is not a bound.
harness_timeout_seconds() {
    local name="$1" default="$2" value
    value="${!name:-${default}}"
    if ! [[ "${value}" =~ ^[1-9][0-9]{0,4}$ ]] || (( value > 86400 )); then
        echo "[run-tests] ${name}='${value}': need whole seconds, 1..86400" >&2
        return 2
    fi
    printf '%s' "${value}"
}

# harness_bounded SECONDS LABEL COMMAND [ARG...]
# Runs COMMAND (an external program -- ssh, scp -- never a shell function,
# whose children a kill would orphan) and gives it SECONDS to finish. On time
# it returns COMMAND's status. Otherwise COMMAND is stopped (TERM, then KILL
# two seconds later), a line naming LABEL, the bound and
# HARNESS_TIMEOUT_CONTEXT (run-tests.sh sets it to the scenario filter) goes
# to stderr, and the status is 124 -- the number coreutils' timeout uses.
#
# Why this exists: a remote command that never returns used to hold the run,
# silently, until CI cancelled the job (rust-fs-ntfs#403, #388). Now it is a
# failure that says which command stopped, for which scenario, and when.
#
# stdin is handed to COMMAND explicitly: bash gives a background job
# /dev/null otherwise, which would starve `tar ... | ssh ... tar -xf -`.
# The watchdog's own output goes to /dev/null so a caller capturing
# COMMAND's output with $(...) is not held open by it. Plain bash 3.2.
harness_bounded() {
    local seconds="$1" label="$2"
    shift 2
    local fired pid watchdog rc=0
    fired="$(mktemp "${TMPDIR:-/tmp}/fswth-bounded.XXXXXX")" || return 1
    rm -f "${fired}"
    "$@" <&0 &
    pid=$!
    (
        deadline=$((SECONDS + seconds))
        while kill -0 "${pid}" 2>/dev/null; do
            if (( SECONDS >= deadline )); then
                : > "${fired}"
                kill -TERM "${pid}" 2>/dev/null
                sleep 2
                kill -KILL "${pid}" 2>/dev/null
                exit 0
            fi
            sleep 1
        done
    ) < /dev/null > /dev/null 2>&1 &
    watchdog=$!
    wait "${pid}" || rc=$?
    kill "${watchdog}" 2>/dev/null || true
    wait "${watchdog}" 2>/dev/null || true
    if [[ -e "${fired}" ]]; then
        rm -f "${fired}"
        echo "[run-tests] TIMEOUT: ${label} did not finish within ${seconds}s${HARNESS_TIMEOUT_CONTEXT:+ (${HARNESS_TIMEOUT_CONTEXT})}; stopped it" >&2
        return 124
    fi
    return "${rc}"
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

# harness_self_version
# Echoes a one-line identity for *this* fs-windows-test-harness checkout, derived
# from git in $harness_root. Format: "<describe> (<branch> @ <sha>)".
# `<describe>` is `git describe --tags --always --dirty`, so a clean tag
# shows as e.g. "v2.0.0", a few commits past as "v2.0.0-5-g2e4a610", and
# a working-tree with uncommitted changes appends "-dirty". Branch is
# "detached" when HEAD isn't on a named branch (e.g. checked out at a
# tag). Falls back to "unknown" outside a git checkout.
harness_self_version() {
    if ! command -v git >/dev/null 2>&1; then
        printf 'unknown (git not available)'
        return
    fi
    if ! git -C "${harness_root}" rev-parse --git-dir >/dev/null 2>&1; then
        printf 'unknown (not a git checkout)'
        return
    fi
    local desc branch sha
    desc=$(git -C "${harness_root}" describe --tags --always --dirty 2>/dev/null || echo "unknown")
    branch=$(git -C "${harness_root}" rev-parse --abbrev-ref HEAD 2>/dev/null || echo "?")
    sha=$(git -C "${harness_root}" rev-parse --short HEAD 2>/dev/null || echo "?")
    [[ "${branch}" == "HEAD" ]] && branch="detached"
    printf '%s (%s @ %s)' "${desc}" "${branch}" "${sha}"
}
