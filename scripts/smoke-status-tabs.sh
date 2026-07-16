#!/bin/sh

# Open one Zellij tab for each visible Zag Lens state.

set -eu

fail() {
    printf '%s\n' "zag-lens status smoke: $*" >&2
    exit 1
}

command -v zellij >/dev/null 2>&1 || fail "zellij is not available"
command -v awk >/dev/null 2>&1 || fail "awk is not available"
[ -n "${ZELLIJ_SESSION_NAME:-}" ] || fail "run this script inside Zellij"

epoch_hex="$(printf '%08x' "$(date +%s)")"
process_hex="$(printf '%04x' "$(( $$ % 65536 ))")"
run_id="${epoch_hex}${process_hex}"
event_number=0

open_status_tab() {
    status="$1"
    event_kind="$2"
    canonical_state="$3"
    occurred_at="$4"

    tab_id="$(zellij action new-tab --name "zag-lens: ${status}")"
    case "${tab_id}" in
        *[!0-9]* | "") fail "could not determine tab ID for ${status}" ;;
    esac

    # `new-tab` returns a stable tab ID, not its terminal pane ID. Resolve the
    # new tab's terminal from application state before delivering the event.
    protocol_pane_id=""
    attempts=0
    while [ -z "${protocol_pane_id}" ] && [ "${attempts}" -lt 40 ]; do
        protocol_pane_id="$(
            zellij action list-panes --tab | awk -v wanted="${tab_id}" '
                NR > 1 && $1 == wanted {
                    for (field = 2; field <= NF; field++) {
                        if ($field ~ /^terminal_[0-9]+$/ &&
                                $(field + 1) == "terminal") {
                            print $field
                            exit
                        }
                    }
                }
            '
        )"
        attempts=$((attempts + 1))
        [ -n "${protocol_pane_id}" ] || sleep 0.05
    done
    [ -n "${protocol_pane_id}" ] || \
        fail "could not determine terminal pane ID for ${status}"

    event_number=$((event_number + 1))
    event_id="${epoch_hex}-${process_hex}-4000-8000-$(printf '%012x' "${event_number}")"
    identity="status-smoke-${run_id}-${status}"
    payload="$(printf '%s' \
        "{\"schema_version\":2," \
        "\"event_id\":\"${event_id}\"," \
        "\"occurred_at\":\"${occurred_at}\"," \
        "\"harness\":\"status-smoke\"," \
        "\"native_event\":\"status-smoke-${status}\"," \
        "\"kind\":\"${event_kind}\"," \
        "\"state\":\"${canonical_state}\"," \
        "\"session_id\":\"${identity}\"," \
        "\"agent_instance_id\":\"${identity}\"," \
        "\"turn_id\":\"status-smoke-turn\"," \
        "\"pane_id\":\"${protocol_pane_id}\"," \
        "\"adapter\":{\"name\":\"status-smoke\",\"version\":1}}")"

    zellij pipe --name zag-lens:event -- "${payload}"
}

now="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"

open_status_tab "working" "turn_started" "working" "${now}"
open_status_tab \
    "waiting_for_user" "interaction_required" "waiting_for_user" "${now}"
open_status_tab "succeeded" "turn_completed" "succeeded" "${now}"
open_status_tab "failed" "turn_failed" "failed" "${now}"

# Stale is timer-derived rather than accepted directly by the event protocol.
open_status_tab "stale" "turn_started" "working" "1970-01-01T00:00:00Z"
