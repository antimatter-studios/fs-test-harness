#!/usr/bin/env bash
# scripts/host/verify-tree.sh -- generic v2 op: assert against a recursive
# tree listing produced by the consumer binary's `tree` subcommand.
#
# Output contract:
#   `<bin> tree <image>` writes free-form text describing the volume's
#   recursive structure. Format is consumer-defined (tree art, indented
#   listing, whatever) — this script doesn't parse it. The only
#   assertion shape is sha256 of the raw output.
#
# Comparison flag:
#   --expect-stdout-sha256 <H>    sha256 hex of the raw stdout
#
# Required:
#   --binary <PATH>
#
# Positional:
#   <image>

set -euo pipefail

binary=""
expect_sha256=""
positional=()
extra=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --binary)               binary="$2"; shift 2 ;;
        --expect-stdout-sha256) expect_sha256="$2"; shift 2 ;;
        --)                     shift; extra=("$@"); break ;;
        --*)                    echo "verify-tree: unknown flag: $1" >&2; exit 2 ;;
        *)                      positional+=("$1"); shift ;;
    esac
done

if [[ -z "${binary}" ]]; then
    echo "verify-tree: --binary <path> is required" >&2; exit 2
fi
if [[ "${#positional[@]}" -lt 1 ]]; then
    echo "verify-tree: missing <image>" >&2; exit 2
fi
if [[ ! -x "${binary}" ]]; then
    echo "verify-tree: binary not executable: ${binary}" >&2; exit 2
fi

image="${positional[0]}"

tmp=$(mktemp -t verify-tree.XXXXXX)
trap 'rm -f "${tmp}"' EXIT
"${binary}" tree "${image}" ${extra[@]+"${extra[@]}"} > "${tmp}"

if [[ -n "${expect_sha256}" ]]; then
    got=$(shasum -a 256 < "${tmp}" | awk '{print $1}')
    if [[ "${got}" != "${expect_sha256}" ]]; then
        echo "verify-tree: stdout sha256 mismatch:" >&2
        echo "  got:  ${got}" >&2
        echo "  want: ${expect_sha256}" >&2
        exit 1
    fi
fi

exit 0
