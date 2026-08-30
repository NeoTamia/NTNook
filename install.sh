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

for command in curl tar sha256sum install sed grep; do
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

quote_shell_path() {
    printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"
}

append_bash_completion() {
    nook_rc_file=$1
    nook_completion_file=$2
    nook_quoted_file=$(quote_shell_path "$nook_completion_file")
    if [ -f "$nook_rc_file" ] && grep -Fq '# >>> nook completions >>>' "$nook_rc_file"; then
        return
    fi
    {
        printf '\n# >>> nook completions >>>\n'
        printf 'if [ -r %s ]; then\n' "$nook_quoted_file"
        printf '    . %s\n' "$nook_quoted_file"
        printf 'fi\n'
        printf '# <<< nook completions <<<\n'
    } >> "$nook_rc_file"
}

append_zsh_completion() {
    nook_rc_file=$1
    nook_completion_directory=$2
    nook_completion_file=$3
    nook_quoted_directory=$(quote_shell_path "$nook_completion_directory")
    nook_quoted_file=$(quote_shell_path "$nook_completion_file")
    if [ -f "$nook_rc_file" ] && grep -Fq '# >>> nook completions >>>' "$nook_rc_file"; then
        return
    fi
    {
        printf '\n# >>> nook completions >>>\n'
        printf 'if [[ -r %s ]]; then\n' "$nook_quoted_file"
        printf '    fpath=(%s $fpath)\n' "$nook_quoted_directory"
        printf '    autoload -Uz compinit\n'
        printf '    if (( ! $+functions[compdef] )); then\n'
        printf '        compinit\n'
        printf '    fi\n'
        printf '    source %s\n' "$nook_quoted_file"
        printf 'fi\n'
        printf '# <<< nook completions <<<\n'
    } >> "$nook_rc_file"
}

if [ -n "${HOME:-}" ]; then
    completion_data_home=${XDG_DATA_HOME:-"$HOME/.local/share"}
    bash_completion_directory="$completion_data_home/bash-completion/completions"
    zsh_completion_directory="$completion_data_home/zsh/site-functions"
    bash_completion_file="$bash_completion_directory/nook"
    zsh_completion_file="$zsh_completion_directory/_nook"

    mkdir -p "$bash_completion_directory" "$zsh_completion_directory"
    "${install_directory}/nook" completions bash > "${temporary_directory}/nook.bash"
    "${install_directory}/nook" completions zsh > "${temporary_directory}/nook.zsh"
    install -m 0644 "${temporary_directory}/nook.bash" "$bash_completion_file"
    install -m 0644 "${temporary_directory}/nook.zsh" "$zsh_completion_file"

    append_bash_completion "$HOME/.bashrc" "$bash_completion_file"
    append_zsh_completion "$HOME/.zshrc" "$zsh_completion_directory" "$zsh_completion_file"
    echo "nook: installed Bash and Zsh completions"
else
    echo "nook: HOME is unset; shell completions were not installed" >&2
fi

echo "nook: installed ${install_directory}/nook"
case ":${PATH}:" in
    *":${install_directory}:"*) ;;
    *) echo "nook: add ${install_directory} to PATH to invoke nook" ;;
esac
