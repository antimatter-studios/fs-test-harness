#!/usr/bin/env bash
# scripts/host/verify-parts.sh -- generic v2 op: assert against partition-
# table inspection by the consumer binary.
#
# Output contract:
#   `<bin> parts <image>` writes free-form text describing the partition
#   table (MBR / GPT / no-table). Format is consumer-defined; this
#   script only does substring matching against the raw stdout.
#
#   Some images legitimately have NO partition table (raw filesystem
#   images). Consumers typically signal that by exiting non-zero from
#   `parts` — let the harness's expect_exit machinery handle the
#   exit-code assertion (use a separate [ops.verify-parts-fails] op-def
#   with `expect_exit = 1`). This script PASSES THROUGH whatever exit
#   code the binary returned, so it composes cleanly with that pattern.
#
# Comparison flag:
#   --expect-stdout-contains <S>  fixed-string substring of stdout
#                                 (repeatable for multiple needles)
#
# Required:
#   --binary <PATH>
#
# Positional:
#   <image>

set -uo pipefail

binary=""
expect_contains=()
positional=()
extra=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --binary)                  binary="$2"; shift 2 ;;
        --expect-stdout-contains)  expect_contains+=("$2"); shift 2 ;;
        --)                        shift; extra=("$@"); break ;;
        --*)                       echo "verify-parts: unknown flag: $1" >&2; exit 2 ;;
        *)                         positional+=("$1"); shift ;;
    esac
done

if [[ -z "${binary}" ]]; then
    echo "verify-parts: --binary <path> is required" >&2; exit 2
fi
if [[ "${#positional[@]}" -lt 1 ]]; then
    echo "verify-parts: missing <image>" >&2; exit 2
fi
if [[ ! -x "${binary}" ]]; then
    echo "verify-parts: binary not executable: ${binary}" >&2; exit 2
fi

image="${positional[0]}"

# Capture both stdout + exit. Don't fail on non-zero — the harness's
# expect_exit handles that judgement (see header).
out=$("${binary}" parts "${image}" ${extra[@]+"${extra[@]}"} 2>&1)
rc=$?

for needle in ${expect_contains[@]+"${expect_contains[@]}"}; do
    if ! echo "${out}" | grep -qF "${needle}"; then
        echo "verify-parts: stdout missing expected substring: '${needle}'" >&2
        echo "got stdout (rc=${rc}):" >&2
        echo "${out}" | sed 's/^/  /' >&2
        exit 1
    fi
done

exit "${rc}"
