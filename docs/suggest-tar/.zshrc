# ── Bomb Defusal Demo for Shell-AI ────────────────────────
#
# A timed Zsh prompt demo for Shell-AI's Ctrl+G keybinding.
# The banner counts down from TEN to ZERO. The bomb is
# disarmed if a command starting with the target word
# appears in the input — either from the AI or typed
# manually — before time runs out.
#
# Usage:
#   ZDOTDIR=/path/to/dir-containing-this-file zsh
#
# Requires: shell-ai (for Ctrl+G keybinding)

setopt PROMPT_SUBST

# ── Configuration ──────────────────────────────────────────
# The banner reads:
#   "${_bomb_pre} <target> ${_bomb_post} <WORD> SECONDS."

typeset -g  _bomb_pre="TO DISARM THE BOMB, SIMPLY ENTER A VALID"
typeset -g  _bomb_target=tar
typeset -g  _bomb_post="COMMAND ON YOUR FIRST TRY. NO GOOGLING. YOU HAVE"
typeset -g  _bomb_duration=10        # countdown seconds (max 10)
typeset -g  _bomb_urgent=3           # threshold to turn red
typeset -ga _bomb_words=(ZERO ONE TWO THREE FOUR FIVE SIX SEVEN EIGHT NINE TEN)

# Visible char count of the fixed portion: "pre target post "
typeset -g _bomb_fixed_len=$(( ${#_bomb_pre} + ${#_bomb_target} + ${#_bomb_post} + 3 ))

# ── State ──────────────────────────────────────────────────

typeset -g _bomb_remaining=$_bomb_duration
typeset -g _bomb_bg=0
typeset -g _shai_active=0            # 1 while the Shell-AI widget is running

# ── Core ───────────────────────────────────────────────────

_bomb_defuse() {
    _bomb_remaining=-1
    (( _bomb_bg )) && kill $_bomb_bg 2>/dev/null
    _bomb_bg=0
}

# ── Prompt (ZLE-managed, used outside the shimmer) ─────────

_bomb_prompt() {
    if (( _bomb_remaining == -1 )); then
        print "✅ THE BOMB HAS BEEN DISARMED. ✅"
        print -n "~# "
    elif (( _bomb_remaining <= 0 )); then
        print "💥 THE BOMB HAS DETONATED. 💥"
        print -n "💥 ~# "
    else
        local word=${_bomb_words[$_bomb_remaining+1]} s=SECONDS
        (( _bomb_remaining == 1 )) && s=SECOND
        if (( _bomb_remaining <= _bomb_urgent )); then
            print "${_bomb_pre} %F{240}${_bomb_target}%f ${_bomb_post} %F{red}%S${word}%s%f ${s}."
        else
            print "${_bomb_pre} %F{240}${_bomb_target}%f ${_bomb_post} %S${word}%s ${s}."
        fi
        print -n "~# "
    fi
}

PROMPT='$(_bomb_prompt)'

# ── Direct Banner Update (bypasses ZLE during shimmer) ─────
# Rewrites the banner in-place using DEC cursor save/restore
# so the shimmer animation line below is never disturbed.

_bomb_banner_update() {
    local word=${_bomb_words[$_bomb_remaining+1]} s=SECONDS
    (( _bomb_remaining == 1 )) && s=SECOND
    local len=$(( _bomb_fixed_len + ${#word} + ${#s} + 2 ))
    local up=$(( (len + ${COLUMNS:-80} - 1) / ${COLUMNS:-80} ))
    printf '\e7\e[%dA\r\e[0m' "$up"
    printf '%s \e[38;5;240m%s\e[0m %s ' "$_bomb_pre" "$_bomb_target" "$_bomb_post"
    if (( _bomb_remaining <= _bomb_urgent )); then
        printf '\e[31m\e[7m%s\e[0m' "$word"
    else
        printf '\e[7m%s\e[0m' "$word"
    fi
    printf ' %s.\e[K\e8' "$s"
}

# ── Countdown Timer ────────────────────────────────────────

TRAPUSR1() {
    (( _bomb_remaining <= 0 )) && return
    (( _bomb_remaining-- ))
    if (( _shai_active )); then
        (( _bomb_remaining > 0 )) && _bomb_banner_update
    else
        zle reset-prompt 2>/dev/null
    fi
}

{ for i in {1..$_bomb_duration}; do sleep 1; kill -USR1 $$ 2>/dev/null || break; done } &!
_bomb_bg=$!

# ── Cleanup ────────────────────────────────────────────────
# Reset to a plain prompt after any command execution.

preexec() {
    (( _bomb_bg )) && kill $_bomb_bg 2>/dev/null
    _bomb_bg=0
    _shai_active=0
    PROMPT='~# '
    unfunction TRAPUSR1 _bomb_prompt _bomb_defuse \
               _bomb_banner_update preexec 2>/dev/null
}

# ── Shell-AI Integration ──────────────────────────────────

eval "$(shell-ai integration generate zsh --preset=full --stdout)"

# Wrap the Shell-AI widget to (a) flag shimmer-active so
# TRAPUSR1 uses direct banner updates instead of
# zle reset-prompt, and (b) auto-disarm if the AI suggests
# a command starting with the target word.
functions[_shai_transform_orig]=$functions[_shai_transform]
_shai_transform() {
    _shai_active=1
    _shai_transform_orig "$@"
    _shai_active=0
    if (( _bomb_remaining > 0 )) && [[ "$BUFFER" == ${_bomb_target}[[:space:]]* ]]; then
        _bomb_defuse
        zle reset-prompt
    fi
}

# Intercept Enter: disarm on target command without executing.
_bomb_accept_line() {
    if (( _bomb_remaining > 0 )) && [[ "$BUFFER" == ${_bomb_target}[[:space:]]* || "$BUFFER" == $_bomb_target ]]; then
        _bomb_defuse
        zle reset-prompt
        return
    fi
    zle .accept-line
}
zle -N accept-line _bomb_accept_line