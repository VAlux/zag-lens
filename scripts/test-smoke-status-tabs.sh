#!/bin/sh

# Regression test for tab-ID to terminal-pane-ID resolution in the smoke script.

set -eu

script_directory="$(CDPATH='' cd "$(dirname "$0")" && pwd)"
test_directory="$(mktemp -d "${TMPDIR:-/tmp}/zag-lens-status-smoke-test.XXXXXX")"
mock_bin="${test_directory}/bin"
zellij_state="${test_directory}/zellij-state"
pipe_log="${test_directory}/pipe-log"

cleanup() {
    rm -rf "${test_directory}"
}

fail() {
    printf '%s\n' "status smoke script test: $*" >&2
    exit 1
}

trap cleanup 0
trap 'exit 1' HUP INT TERM

mkdir -p "${mock_bin}"
printf '%s\n' 0 > "${zellij_state}"

cat > "${mock_bin}/zellij" <<'EOF'
#!/bin/sh
set -eu

case "$1 $2" in
    "action new-tab")
        count="$(cat "${TEST_ZELLIJ_STATE}")"
        count=$((count + 1))
        printf '%s\n' "${count}" > "${TEST_ZELLIJ_STATE}"
        printf '%s\n' "${count}"
        ;;
    "action list-panes")
        count="$(cat "${TEST_ZELLIJ_STATE}")"
        printf '%s\n' "TAB_ID TAB_POS TAB_NAME PANE_ID TYPE TITLE"
        index=1
        while [ "${index}" -le "${count}" ]; do
            pane_id=$((index + 100))
            printf '%s\n' \
                "${index} ${index} zag-lens status terminal_${pane_id} terminal shell"
            index=$((index + 1))
        done
        ;;
    "pipe --name")
        for argument in "$@"; do
            payload="${argument}"
        done
        printf '%s\n' "${payload}" >> "${TEST_PIPE_LOG}"
        ;;
    *) exit 1 ;;
esac
EOF
chmod +x "${mock_bin}/zellij"

TEST_PIPE_LOG="${pipe_log}"
TEST_ZELLIJ_STATE="${zellij_state}"
export TEST_PIPE_LOG TEST_ZELLIJ_STATE

PATH="${mock_bin}:${PATH}" ZELLIJ_SESSION_NAME="test-session" \
    sh "${script_directory}/smoke-status-tabs.sh"

[ "$(wc -l < "${pipe_log}" | tr -d ' ')" -eq 5 ] || \
    fail "expected five pipe deliveries"

index=1
for status in working waiting_for_user succeeded failed stale; do
    pane_id=$((index + 100))
    line="$(sed -n "${index}p" "${pipe_log}")"
    printf '%s\n' "${line}" | \
        grep -F "\"pane_id\":\"terminal_${pane_id}\"" >/dev/null || \
        fail "${status} used the tab ID instead of terminal_${pane_id}"
    printf '%s\n' "${line}" | \
        grep -F "\"native_event\":\"status-smoke-${status}\"" >/dev/null || \
        fail "${status} event was not delivered in order"
    index=$((index + 1))
done

printf '%s\n' "status smoke script test: ok"
