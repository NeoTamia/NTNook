#!/bin/sh
set -eu

repository="NeoTamia/NTNook"
target="x86_64-unknown-linux-musl"
archive="nook-${target}.tar.xz"
checksum="${archive}.sha256"

if [ "$(uname -s)" != "Linux" ]; then
    echo "nook: this installer currently supports Linux only" >&2
    exit 1
fi

case "$(uname -m)" in
    x86_64 | amd64) ;;
    *)
        echo "nook: unsupported architecture $(uname -m); expected x86_64" >&2
        exit 1
        ;;
esac

for command in curl tar sha256sum install; do
    if ! command -v "$command" >/dev/null 2>&1; then
        echo "nook: required command not found: $command" >&2
        exit 1
    fi
done

if [ -n "${NOOK_VERSION:-}" ]; then
    version=${NOOK_VERSION#v}
    base_url="https://github.com/${repository}/releases/download/v${version}"
else
    base_url="https://github.com/${repository}/releases/latest/download"
fi

temporary_directory=$(mktemp -d)
trap 'rm -rf "$temporary_directory"' EXIT HUP INT TERM

curl --proto '=https' --tlsv1.2 --fail --silent --show-error --location \
    "${base_url}/${archive}" --output "${temporary_directory}/${archive}"
curl --proto '=https' --tlsv1.2 --fail --silent --show-error --location \
    "${base_url}/${checksum}" --output "${temporary_directory}/${checksum}"

(
    cd "$temporary_directory"
    sha256sum --check "$checksum"
    tar --extract --xz --file "$archive"
)

install_directory=${NOOK_INSTALL_DIR:-${XDG_BIN_HOME:-"$HOME/.local/bin"}}
mkdir -p "$install_directory"
install -m 0755 "${temporary_directory}/nook" "${install_directory}/nook"

echo "nook: installed ${install_directory}/nook"
case ":${PATH}:" in
    *":${install_directory}:"*) ;;
    *) echo "nook: add ${install_directory} to PATH to invoke nook" ;;
esac
