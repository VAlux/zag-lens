#!/bin/sh

# Deterministic end-to-end test for install.sh using local command doubles.

set -eu

script_directory="$(CDPATH= cd "$(dirname "$0")" && pwd)"
test_directory="$(mktemp -d "${TMPDIR:-/tmp}/zag-lens-install-test.XXXXXX")"
mock_bin="${test_directory}/bin"
installer_log="${test_directory}/installer-arguments"

cleanup() {
    rm -rf "${test_directory}"
}

fail() {
    printf '%s\n' "install script test: $*" >&2
    exit 1
}

trap cleanup 0
trap 'exit 1' HUP INT TERM

mkdir -p "${mock_bin}" "${test_directory}/tmp"

cat > "${mock_bin}/uname" <<'EOF'
#!/bin/sh
case "${1:-}" in
    -s) printf '%s\n' Darwin ;;
    -m) printf '%s\n' arm64 ;;
    *) exit 1 ;;
esac
EOF

cat > "${mock_bin}/curl" <<'EOF'
#!/bin/sh
set -eu

output=""
url=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        -o)
            shift
            output="$1"
            ;;
        http://* | https://*) url="$1" ;;
    esac
    shift
done

case "${url}" in
    */releases/latest)
        printf '%s\n' "https://github.com/VAlux/zag-lens/releases/tag/v9.8.7"
        ;;
    */"${TEST_ARCHIVE_NAME}")
        printf '%s\n' "archive fixture" > "${output}"
        ;;
    */"${TEST_PLUGIN_NAME}")
        printf '%s\n' "plugin fixture" > "${output}"
        ;;
    */SHA256SUMS)
        printf '%s  %s\n' "${TEST_ARCHIVE_HASH}" "${TEST_ARCHIVE_NAME}" \
            > "${output}"
        printf '%s  %s\n' "${TEST_PLUGIN_HASH}" "${TEST_PLUGIN_NAME}" \
            >> "${output}"
        ;;
    *) exit 1 ;;
esac
EOF

cat > "${mock_bin}/tar" <<'EOF'
#!/bin/sh
set -eu

destination=""
while [ "$#" -gt 0 ]; do
    if [ "$1" = "-C" ]; then
        shift
        destination="$1"
    fi
    shift
done

[ -n "${destination}" ] || exit 1
directory="${destination}/${TEST_EXTRACTED_DIRECTORY}"
installer="${directory}/zag-lens"
mkdir -p "${directory}"
{
    printf '%s\n' '#!/bin/sh'
    printf '%s\n' 'printf '\''%s\n'\'' "$@" > "${TEST_INSTALLER_LOG}"'
} > "${installer}"
chmod +x "${installer}"
EOF

chmod +x "${mock_bin}/curl" "${mock_bin}/tar" "${mock_bin}/uname"

TEST_ARCHIVE_NAME="zag-lens-9.8.7-aarch64-apple-darwin.tar.gz"
TEST_PLUGIN_NAME="zag-lens-plugin-9.8.7.wasm"
TEST_EXTRACTED_DIRECTORY="${TEST_ARCHIVE_NAME%.tar.gz}"
if command -v sha256sum >/dev/null 2>&1; then
    TEST_ARCHIVE_HASH="$(printf '%s\n' 'archive fixture' | sha256sum | awk '{print $1}')"
    TEST_PLUGIN_HASH="$(printf '%s\n' 'plugin fixture' | sha256sum | awk '{print $1}')"
else
    TEST_ARCHIVE_HASH="$(printf '%s\n' 'archive fixture' | shasum -a 256 | awk '{print $1}')"
    TEST_PLUGIN_HASH="$(printf '%s\n' 'plugin fixture' | shasum -a 256 | awk '{print $1}')"
fi
TEST_INSTALLER_LOG="${installer_log}"
export TEST_ARCHIVE_HASH TEST_ARCHIVE_NAME TEST_EXTRACTED_DIRECTORY
export TEST_INSTALLER_LOG TEST_PLUGIN_HASH TEST_PLUGIN_NAME

output="$(
    PATH="${mock_bin}:${PATH}" TMPDIR="${test_directory}/tmp" \
        sh "${script_directory}/install.sh" --zellij --codex
)"

printf '%s\n' "${output}" | grep -F \
    "zag-lens install: installed Zag Lens 9.8.7" >/dev/null || \
    fail "success output is missing"

[ "$(sed -n '1p' "${installer_log}")" = "setup" ] || \
    fail "setup subcommand was not forwarded"
[ "$(sed -n '2p' "${installer_log}")" = "--plugin-wasm" ] || \
    fail "plugin option was not forwarded"
case "$(sed -n '3p' "${installer_log}")" in
    */"${TEST_PLUGIN_NAME}") ;;
    *) fail "plugin path was not forwarded" ;;
esac
[ "$(sed -n '4p' "${installer_log}")" = "--zellij" ] || \
    fail "Zellij selector was not forwarded"
[ "$(sed -n '5p' "${installer_log}")" = "--codex" ] || \
    fail "Codex selector was not forwarded"
[ "$(sed -n '6p' "${installer_log}")" = "" ] || \
    fail "unexpected installer argument"

if find "${test_directory}/tmp" -mindepth 1 -print -quit | grep -q .; then
    fail "temporary download directory was not removed"
fi

printf '%s\n' "install script test: ok"
