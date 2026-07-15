#!/bin/sh

# Install the latest prebuilt Zag Lens release without requiring Rust.

set -eu

repository="VAlux/zag-lens"
releases_url="https://github.com/${repository}/releases"
temporary_directory=""

say() {
    printf '%s\n' "zag-lens install: $*"
}

fail() {
    printf '%s\n' "zag-lens install: $*" >&2
    exit 1
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || fail "required command not found: $1"
}

cleanup() {
    if [ -n "${temporary_directory}" ] && [ -d "${temporary_directory}" ]; then
        rm -rf "${temporary_directory}"
    fi
}

trap cleanup 0
trap 'exit 1' HUP INT TERM

for command_name in curl tar uname mktemp awk; do
    require_command "${command_name}"
done

platform="$(uname -s)/$(uname -m)"
case "${platform}" in
    Darwin/arm64) target="aarch64-apple-darwin" ;;
    Darwin/x86_64) target="x86_64-apple-darwin" ;;
    Linux/aarch64 | Linux/arm64) target="aarch64-unknown-linux-gnu" ;;
    Linux/x86_64) target="x86_64-unknown-linux-gnu" ;;
    *) fail "unsupported platform: ${platform}" ;;
esac

latest_url="$(
    curl -fsSL -o /dev/null -w '%{url_effective}' "${releases_url}/latest"
)"
version="${latest_url##*/v}"
case "${version}" in
    "" | *[!0-9A-Za-z.-]*) fail "could not resolve the latest release version" ;;
esac

archive="zag-lens-${version}-${target}.tar.gz"
plugin="zag-lens-plugin-${version}.wasm"
release_url="${releases_url}/download/v${version}"
temporary_directory="$(mktemp -d "${TMPDIR:-/tmp}/zag-lens-install.XXXXXX")"
checksums="${temporary_directory}/SHA256SUMS"
selected_checksums="${temporary_directory}/selected-SHA256SUMS"

say "downloading Zag Lens ${version} for ${target}"
curl -fL --retry 3 -o "${temporary_directory}/${archive}" \
    "${release_url}/${archive}"
curl -fL --retry 3 -o "${temporary_directory}/${plugin}" \
    "${release_url}/${plugin}"
curl -fL --retry 3 -o "${checksums}" "${release_url}/SHA256SUMS"

awk -v archive="${archive}" -v plugin="${plugin}" '
    $2 == archive || $2 == plugin { print }
' "${checksums}" > "${selected_checksums}"

checksum_count="$(awk 'END { print NR }' "${selected_checksums}")"
[ "${checksum_count}" -eq 2 ] || fail "release checksums are incomplete"

say "verifying release checksums"
if command -v sha256sum >/dev/null 2>&1; then
    (cd "${temporary_directory}" && sha256sum -c "${selected_checksums}")
elif command -v shasum >/dev/null 2>&1; then
    (cd "${temporary_directory}" && shasum -a 256 -c "${selected_checksums}")
else
    fail "required checksum command not found: sha256sum or shasum"
fi

tar -xzf "${temporary_directory}/${archive}" -C "${temporary_directory}"
installer="${temporary_directory}/zag-lens-${version}-${target}/zag-lens"
[ -f "${installer}" ] || fail "release archive does not contain zag-lens"
[ -x "${installer}" ] || fail "release archive contains a non-executable zag-lens"

say "installing user-level files and configuration"
"${installer}" setup \
    --plugin-wasm "${temporary_directory}/${plugin}" \
    "$@"

say "installed Zag Lens ${version}"
say "Codex and Claude Code integrations are configured by default"
say "restart Zellij and approve the requested permissions"
say "Codex users: inspect and trust the Zag Lens commands with /hooks"
