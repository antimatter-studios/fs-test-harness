#!/usr/bin/env bash
# scripts/host/verify-cat.sh -- generic v2 op: assert against file content
# read by the consumer binary's `cat` subcommand.
#
# Output contract:
#   `<bin> cat <image> <path>` writes the file's RAW BYTES to stdout
#   (no framing, no trailing newline injected, no encoding wrapping).
#
# Comparison flags (any combination, all optional):
#   --expect-size <N>          byte length of the cat output
#   --expect-sha256 <H>        sha256 hex of the cat output
#   --expect-stdout-sha256 <H> alias for --expect-sha256 (matrix-author
#                              hygiene; some scenarios distinguish
#                              "this is the file content's hash" vs
#                              "this is the binary's stdout hash" but
#                              for `cat` they're the same thing)
#   --expect-content <STR>     byte-exact string compare. Use sparingly —
#                              JSON-escaped newlines in <STR> survive
#                              shell quoting only by single-quote luck.
#                              Prefer --expect-size + --expect-sha256
#                              for any content with newlines or non-
#                              ASCII bytes.
#
# Required:
#   --binary <PATH>
#
# Positional:
#   <image> <path>
#
# Exit: 0 = all checks pass; 1 = one or more drift; 2 = invocation error.

set -euo pipefail

binary=""
expect_size=""
expect_sha256=""
expect_content=""
positional=()
extra=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --binary)               binary="$2"; shift 2 ;;
        --expect-size)          expect_size="$2"; shift 2 ;;
        --expect-sha256|--expect-stdout-sha256) expect_sha256="$2"; shift 2 ;;
        --expect-content)       expect_content="$2"; shift 2 ;;
        --)                     shift; extra=("$@"); break ;;
        --*)                    echo "verify-cat: unknown flag: $1" >&2; exit 2 ;;
        *)                      positional+=("$1"); shift ;;
    esac
done

if [[ -z "${binary}" ]]; then
    echo "verify-cat: --binary <path> is required" >&2; exit 2
fi
if [[ "${#positional[@]}" -lt 2 ]]; then
    echo "verify-cat: missing <image> <path> positional args" >&2; exit 2
fi
if [[ ! -x "${binary}" ]]; then
    echo "verify-cat: binary not executable: ${binary}" >&2; exit 2
fi

image="${positional[0]}"
path="${positional[1]}"

# Tempfile capture (preserves trailing newlines that $(...) eats —
# matters for both sha256 and byte-count).
tmp=$(mktemp -t verify-cat.XXXXXX)
trap 'rm -f "${tmp}"' EXIT
"${binary}" cat "${image}" "${path}" ${extra[@]+"${extra[@]}"} > "${tmp}"

fail=0

if [[ -n "${expect_size}" ]]; then
    got_size=$(wc -c < "${tmp}" | tr -d ' ')
    if [[ "${got_size}" != "${expect_size}" ]]; then
        echo "verify-cat: size mismatch at ${path}: got=${got_size} want=${expect_size}" >&2
        fail=1
    fi
fi
if [[ -n "${expect_sha256}" ]]; then
    got_sha=$(shasum -a 256 < "${tmp}" | awk '{print $1}')
    if [[ "${got_sha}" != "${expect_sha256}" ]]; then
        echo "verify-cat: sha256 mismatch at ${path}: got=${got_sha} want=${expect_sha256}" >&2
        fail=1
    fi
fi
if [[ -n "${expect_content}" ]]; then
    expect_tmp=$(mktemp -t verify-cat-expect.XXXXXX)
    printf '%s' "${expect_content}" > "${expect_tmp}"
    if ! cmp -s "${tmp}" "${expect_tmp}"; then
        echo "verify-cat: content mismatch at ${path}:" >&2
        echo "  got (first 200B):  $(head -c 200 "${tmp}")" >&2
        echo "  want:              ${expect_content}" >&2
        fail=1
    fi
    rm -f "${expect_tmp}"
fi

exit "${fail}"
