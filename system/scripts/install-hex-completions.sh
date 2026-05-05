#!/bin/sh
# install-hex-completions.sh — install hex shell completions idempotently.
# Override: set HEX_BIN env var to point to a different hex binary.
set -uo pipefail

HEX_BIN="${HEX_BIN:-${HEX_DIR:-$HOME}/.hex/bin/hex}"

if [ ! -x "$HEX_BIN" ]; then
    printf 'completions: hex binary not found at %s — skipping\n' "$HEX_BIN" >&2
    exit 0
fi

if ! "$HEX_BIN" completions zsh >/dev/null 2>&1; then
    printf 'completions: "%s completions zsh" failed — skipping\n' "$HEX_BIN" >&2
    exit 0
fi

_install_completion() {
    _ic_shell="$1"
    _ic_target="$2"
    _ic_parent="$(dirname "$_ic_target")"

    if ! mkdir -p "$_ic_parent" 2>/dev/null; then
        printf 'completions (%s): mkdir -p %s failed — skipping\n' "$_ic_shell" "$_ic_parent" >&2
        return 0
    fi

    _ic_tmp="${_ic_target}.tmp.$$"
    if ! "$HEX_BIN" completions "$_ic_shell" > "$_ic_tmp" 2>/dev/null; then
        rm -f "$_ic_tmp"
        printf 'completions (%s): generation failed — skipping\n' "$_ic_shell" >&2
        return 0
    fi

    if [ -f "$_ic_target" ]; then
        if cmp -s "$_ic_target" "$_ic_tmp"; then
            rm -f "$_ic_tmp"
            printf 'completions (%s): up to date\n' "$_ic_shell" >&2
        else
            rm -f "$_ic_tmp"
            printf 'completions (%s): user file differs at %s — leaving untouched\n' "$_ic_shell" "$_ic_target" >&2
        fi
        return 0
    fi

    mv "$_ic_tmp" "$_ic_target"
    printf 'completions (%s): installed\n' "$_ic_shell" >&2
}

# Always install completions for all supported shells
_install_completion zsh  "$HOME/.zfunc/_hex"
_install_completion bash "$HOME/.local/share/bash-completion/completions/hex"
_install_completion fish "$HOME/.config/fish/completions/hex.fish"

# For zsh: ensure ~/.zshrc loads completions from ~/.zfunc
_zshrc="$HOME/.zshrc"
if ! grep -qF '.zfunc' "$_zshrc" 2>/dev/null; then
    printf '\n# hex shell completions\nfpath=($HOME/.zfunc $fpath)\nautoload -Uz compinit && compinit\n' >> "$_zshrc"
    printf 'completions (zsh): added fpath setup to %s\n' "$_zshrc" >&2
fi
