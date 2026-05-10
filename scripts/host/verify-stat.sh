#!/usr/bin/env bash
# scripts/host/verify-stat.sh -- generic v2 op: assert against `stat`
# output produced by the consumer binary.
#
# Output contract:
#   `<bin> stat <image> <path>` writes free-form text to stdout.
#   Convention: one fact per line, formatted as "<key>:    <value>"
#   (case-sensitive key, indented or whitespace-padded value). Lines
#   that don't follow this shape are ignored by --expect-size /
#   --expect-mode parsing but visible to --expect-stdout-contains.
#
# Comparison flags:
#   --expect-size <N>             matches the line "size:   <value>";
#                                 numeric equality on <value>
#   --expect-mode <STR>           matches the line "mode:   <value>";
#                                 string equality (e.g. "0o644")
#   --expect-stdout-contains <S>  substring of the full stdout (case-sensitive,
#                                 fixed-string match — repeat the flag for
#                                 multiple substrings)
#
# Required:
#   --binary <PATH>
#
# Positional:
#   <image> <path>

set -euo pipefail

binary=""
expect_size=""
expect_mode=""
expect_contains=()
positional=()
extra=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --binary)                  binary="$2"; shift 2 ;;
        --expect-size)             expect_size="$2"; shift 2 ;;
        --expect-mode)             expect_mode="$2"; shift 2 ;;
        --expect-stdout-contains)  expect_contains+=("$2"); shift 2 ;;
        --)                        shift; extra=("$@"); break ;;
        --*)                       echo "verify-stat: unknown flag: $1" >&2; exit 2 ;;
        *)                         positional+=("$1"); shift ;;
    esac
done

if [[ -z "${binary}" ]]; then
    echo "verify-stat: --binary <path> is required" >&2; exit 2
fi
if [[ "${#positional[@]}" -lt 2 ]]; then
    echo "verify-stat: missing <image> <path>" >&2; exit 2
fi
if [[ ! -x "${binary}" ]]; then
    echo "verify-stat: binary not executable: ${binary}" >&2; exit 2
fi

image="${positional[0]}"
path="${positional[1]}"

out=$("${binary}" stat "${image}" "${path}" ${extra[@]+"${extra[@]}"})
fail=0

if [[ -n "${expect_size}" ]]; then
    got=$(echo "${out}" | awk '/^size:/ {print $2; exit}')
    if [[ "${got}" != "${expect_size}" ]]; then
        echo "verify-stat: size mismatch at ${path}: got=${got} want=${expect_size}" >&2
        fail=1
    fi
fi
if [[ -n "${expect_mode}" ]]; then
    got=$(echo "${out}" | awk '/^mode:/ {print $2; exit}')
    if [[ "${got}" != "${expect_mode}" ]]; then
        echo "verify-stat: mode mismatch at ${path}: got=${got} want=${expect_mode}" >&2
        fail=1
    fi
fi
for needle in ${expect_contains[@]+"${expect_contains[@]}"}; do
    if ! echo "${out}" | grep -qF "${needle}"; then
        echo "verify-stat: stdout missing expected substring at ${path}: '${needle}'" >&2
        echo "got stdout:" >&2
        echo "${out}" | sed 's/^/  /' >&2
        fail=1
    fi
done

exit "${fail}"
