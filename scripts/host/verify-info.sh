#!/usr/bin/env bash
# scripts/host/verify-info.sh -- generic v2 op: assert against volume-info
# output produced by the consumer binary's `info` subcommand.
#
# Output contract:
#   `<bin> info <image>` writes free-form text describing the volume
#   (label, block size, free space, fs-specific metadata). Format is
#   consumer-defined; this script does substring matching only.
#
# Comparison flag:
#   --expect-stdout-contains <S>  fixed-string substring of stdout
#                                 (repeatable for multiple needles —
#                                 each must be present)
#
# Required:
#   --binary <PATH>
#
# Positional:
#   <image>

set -euo pipefail

binary=""
expect_contains=()
positional=()
extra=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --binary)                  binary="$2"; shift 2 ;;
        --expect-stdout-contains)  expect_contains+=("$2"); shift 2 ;;
        --)                        shift; extra=("$@"); break ;;
        --*)                       echo "verify-info: unknown flag: $1" >&2; exit 2 ;;
        *)                         positional+=("$1"); shift ;;
    esac
done

if [[ -z "${binary}" ]]; then
    echo "verify-info: --binary <path> is required" >&2; exit 2
fi
if [[ "${#positional[@]}" -lt 1 ]]; then
    echo "verify-info: missing <image>" >&2; exit 2
fi
if [[ ! -x "${binary}" ]]; then
    echo "verify-info: binary not executable: ${binary}" >&2; exit 2
fi

image="${positional[0]}"

out=$("${binary}" info "${image}" ${extra[@]+"${extra[@]}"})
fail=0

for needle in ${expect_contains[@]+"${expect_contains[@]}"}; do
    if ! echo "${out}" | grep -qF "${needle}"; then
        echo "verify-info: stdout missing expected substring: '${needle}'" >&2
        echo "got stdout:" >&2
        echo "${out}" | sed 's/^/  /' >&2
        fail=1
    fi
done

exit "${fail}"
