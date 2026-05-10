#!/usr/bin/env bash
# scripts/host/verify-ls.sh -- generic v2 op: assert against a directory
# listing produced by the consumer binary.
#
# Output contract (consumer binary's `<bin> ls <image> <path>` subcommand
# must obey one of these line formats):
#   * "<name>"                — one entry per line, name is the whole line
#   * "<...>... <name>"       — one entry per line, name is the LAST
#                               whitespace-separated token. Lets a
#                               consumer prepend metadata columns
#                               (inode, type, mode) without breaking
#                               this script. Whitespace inside names
#                               isn't supported under the multi-column
#                               form — single-column format is the
#                               escape hatch for those cases.
#
# Comparison flags (any combination):
#   --expect-name <NAME>       repeatable; set-membership check (order
#                              and duplicates ignored)
#   --expect-count <N>         total entry count
#   --expect-stdout-sha256 <H> sha256 of the raw `ls` output bytes
#
# Required:
#   --binary <PATH>            the consumer's binary
#
# Positional (after flags):
#   <image> <path>             passed verbatim to `<bin> ls <image> <path>`
#                              plus any extra flags after `--`
#
# Exit:
#   0  all configured expectations satisfied
#   1  one or more checks failed (drift dumped to stderr)
#   2  invocation error (missing --binary, etc.)

set -euo pipefail

binary=""
expect_names=()
expect_count=""
expect_sha256=""
positional=()
extra=()

# Two-phase parse: collect everything into positional/flags first,
# `--` ends the flag phase and the rest is forwarded to the binary
# as extra ls args (e.g. `--part 1` for whole-disk images).
while [[ $# -gt 0 ]]; do
    case "$1" in
        --binary)               binary="$2"; shift 2 ;;
        --expect-name)          expect_names+=("$2"); shift 2 ;;
        --expect-count)         expect_count="$2"; shift 2 ;;
        --expect-stdout-sha256) expect_sha256="$2"; shift 2 ;;
        --)                     shift; extra=("$@"); break ;;
        --*)
            echo "verify-ls: unknown flag: $1" >&2; exit 2 ;;
        *)
            positional+=("$1"); shift ;;
    esac
done

if [[ -z "${binary}" ]]; then
    echo "verify-ls: --binary <path> is required" >&2; exit 2
fi
if [[ "${#positional[@]}" -lt 2 ]]; then
    echo "verify-ls: missing <image> <path> positional args" >&2; exit 2
fi
if [[ ! -x "${binary}" ]]; then
    echo "verify-ls: binary not executable: ${binary}" >&2; exit 2
fi

image="${positional[0]}"
path="${positional[1]}"

# Capture once; tempfile preserves trailing newlines (which $(...) eats),
# important for stable sha256 comparison.
tmp=$(mktemp -t verify-ls.XXXXXX)
trap 'rm -f "${tmp}"' EXIT
"${binary}" ls "${image}" "${path}" ${extra[@]+"${extra[@]}"} > "${tmp}"

fail=0

# Optional sha256 of raw output.
if [[ -n "${expect_sha256}" ]]; then
    got=$(shasum -a 256 < "${tmp}" | awk '{print $1}')
    if [[ "${got}" != "${expect_sha256}" ]]; then
        echo "verify-ls: stdout sha256 mismatch at ${path}:" >&2
        echo "  got:  ${got}" >&2
        echo "  want: ${expect_sha256}" >&2
        fail=1
    fi
fi

# Structural checks. Read names from the last whitespace token of each
# non-empty line so single-column AND multi-column outputs both work.
if [[ ${#expect_names[@]} -gt 0 || -n "${expect_count}" ]]; then
    got_names=()
    while IFS= read -r line; do
        [[ -z "${line}" ]] && continue
        # last token = name
        name="${line##* }"
        got_names+=("${name}")
    done < "${tmp}"

    if [[ -n "${expect_count}" ]]; then
        if [[ "${#got_names[@]}" -ne "${expect_count}" ]]; then
            echo "verify-ls: count mismatch at ${path}: got=${#got_names[@]} want=${expect_count}" >&2
            fail=1
        fi
    fi
    if [[ ${#expect_names[@]} -gt 0 ]]; then
        # Set-equality. sort+comm keeps it readable + portable to bash 3.2.
        want_sorted=$(printf '%s\n' "${expect_names[@]}" | sort -u)
        got_sorted=$(printf '%s\n' "${got_names[@]}" | sort -u)
        missing=$(comm -23 <(echo "${want_sorted}") <(echo "${got_sorted}"))
        extra_n=$(comm -13 <(echo "${want_sorted}") <(echo "${got_sorted}"))
        if [[ -n "${missing}" || -n "${extra_n}" ]]; then
            echo "verify-ls: name-set drift at ${path}:" >&2
            [[ -n "${missing}"  ]] && echo "  missing: $(echo "${missing}" | tr '\n' ' ')" >&2
            [[ -n "${extra_n}"  ]] && echo "  unexpected: $(echo "${extra_n}" | tr '\n' ' ')" >&2
            fail=1
        fi
    fi
fi

exit "${fail}"
