#!/usr/bin/env bash
# scripts/host/verify-readlink.sh -- assert against the symlink target
# printed by the consumer binary's `readlink` subcommand.
#
# Output contract:
#   `<bin> readlink <image> <path>` prints the symlink target + newline.
#
# Comparison flags:
#   --expect-target <STR>  exact target string (no trailing newline)
#
# Required:
#   --binary <PATH>
#
# Positional:
#   <image> <path>
#
# Exit: 0 = pass; 1 = drift; 2 = invocation error.

set -euo pipefail

binary=""
expect_target=""
positional=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --binary)          binary="$2"; shift 2 ;;
        --expect-target)   expect_target="$2"; shift 2 ;;
        --*)               echo "verify-readlink: unknown flag: $1" >&2; exit 2 ;;
        *)                 positional+=("$1"); shift ;;
    esac
done

if [[ -z "${binary}" ]]; then
    echo "verify-readlink: --binary <path> is required" >&2; exit 2
fi
if [[ "${#positional[@]}" -lt 2 ]]; then
    echo "verify-readlink: missing <image> <path> positional args" >&2; exit 2
fi
if [[ ! -x "${binary}" ]]; then
    echo "verify-readlink: binary not executable: ${binary}" >&2; exit 2
fi

image="${positional[0]}"
path="${positional[1]}"

tmp=$(mktemp -t verify-readlink.XXXXXX)
trap 'rm -f "${tmp}"' EXIT
"${binary}" readlink "${image}" "${path}" > "${tmp}"

fail=0
if [[ -n "${expect_target}" ]]; then
    got=$(tr -d '\n' < "${tmp}")
    if [[ "${got}" != "${expect_target}" ]]; then
        echo "verify-readlink: target mismatch at ${path}: got='${got}' want='${expect_target}'" >&2
        fail=1
    fi
fi
exit "${fail}"
