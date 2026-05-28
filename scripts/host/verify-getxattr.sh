#!/usr/bin/env bash
# scripts/host/verify-getxattr.sh -- generic v2 op: assert against one
# extended-attribute value read by the consumer binary's `getxattr` subcommand.
#
# Output contract:
#   `<bin> getxattr <image> <path> <name>` writes the raw attribute bytes to
#   stdout (no framing, no trailing bytes added).
#
# Comparison flags (any combination, all optional):
#   --expect-size <N>    byte length of the value
#   --expect-sha256 <H>  sha256 hex of the value
#   --expect-content <S> byte-exact string compare (ASCII values only)
#
# Required:
#   --binary <PATH>
#
# Positional:
#   <image> <path> <name>
#
# Exit: 0 = all checks pass; 1 = one or more drift; 2 = invocation error.

set -euo pipefail

binary=""
expect_size=""
expect_sha256=""
expect_content=""
positional=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --binary)          binary="$2"; shift 2 ;;
        --expect-size)     expect_size="$2"; shift 2 ;;
        --expect-sha256)   expect_sha256="$2"; shift 2 ;;
        --expect-content)  expect_content="$2"; shift 2 ;;
        --*)               echo "verify-getxattr: unknown flag: $1" >&2; exit 2 ;;
        *)                 positional+=("$1"); shift ;;
    esac
done

if [[ -z "${binary}" ]]; then
    echo "verify-getxattr: --binary <path> is required" >&2; exit 2
fi
if [[ "${#positional[@]}" -lt 3 ]]; then
    echo "verify-getxattr: missing <image> <path> <name> positional args" >&2; exit 2
fi
if [[ ! -x "${binary}" ]]; then
    echo "verify-getxattr: binary not executable: ${binary}" >&2; exit 2
fi

image="${positional[0]}"
path="${positional[1]}"
name="${positional[2]}"

tmp=$(mktemp -t verify-getxattr.XXXXXX)
trap 'rm -f "${tmp}"' EXIT
"${binary}" getxattr "${image}" "${path}" "${name}" > "${tmp}"

fail=0

if [[ -n "${expect_size}" ]]; then
    got_size=$(wc -c < "${tmp}" | tr -d ' ')
    if [[ "${got_size}" != "${expect_size}" ]]; then
        echo "verify-getxattr: size mismatch ${path}#${name}: got=${got_size} want=${expect_size}" >&2
        fail=1
    fi
fi
if [[ -n "${expect_sha256}" ]]; then
    got_sha=$(shasum -a 256 < "${tmp}" | awk '{print $1}')
    if [[ "${got_sha}" != "${expect_sha256}" ]]; then
        echo "verify-getxattr: sha256 mismatch ${path}#${name}: got=${got_sha} want=${expect_sha256}" >&2
        fail=1
    fi
fi
if [[ -n "${expect_content}" ]]; then
    expect_tmp=$(mktemp -t verify-getxattr-expect.XXXXXX)
    printf '%s' "${expect_content}" > "${expect_tmp}"
    if ! cmp -s "${tmp}" "${expect_tmp}"; then
        echo "verify-getxattr: content mismatch ${path}#${name}:" >&2
        echo "  got (hex):  $(xxd "${tmp}" | head -3)" >&2
        echo "  want:       ${expect_content}" >&2
        fail=1
    fi
    rm -f "${expect_tmp}"
fi

exit "${fail}"
