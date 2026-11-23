if [ -n "${ZSH_VERSION-}" ] && [[ -o interactive ]]; then
    __w11term_enable_zsh_integration() {
        local prompt_percent=0
        [[ -o prompt_percent ]] && prompt_percent=1
        emulate -L zsh

        if (( ! prompt_percent )); then
            builtin printf '%s\n' 'Zsh command recording requires the PROMPT_PERCENT option.' >&2
            return 0
        fi
        if (( ${precmd_functions[(I)__w11term_zsh_prompt]} )); then
            return 0
        fi
        if [[ ${(t)PS1} == *readonly* || ${(t)precmd_functions} == *readonly* ||
              ${(t)preexec_functions} == *readonly* ]]; then
            builtin printf '%s\n' 'Zsh command recording needs a writable prompt and prompt hooks.' >&2
            return 0
        fi
        if [[ $PS1 == *$'\033]133;'* || $PS1 == *$'\033]633;'* ]]; then
            builtin printf '%s\n' 'The Zsh prompt already supplies semantic markers.' >&2
            return 0
        fi

        typeset -gi __w11term_zsh_running=0
        typeset -g __w11term_zsh_input_mark=$'%{\033]133;B\007%}'

        __w11term_zsh_finish() {
            local exit_code=$?
            if (( __w11term_zsh_running )) && ! builtin zle; then
                __w11term_zsh_running=0
                builtin printf '\033]133;D;%d\007' "$exit_code"
            fi
            return 0
        }

        __w11term_zsh_prompt() {
            if builtin zle; then
                return 0
            fi
            builtin printf '\033]133;A\007'
            if [[ -o prompt_percent && ${(t)PS1} != *readonly* &&
                  $PS1 != *"$__w11term_zsh_input_mark" ]]; then
                PS1+=$__w11term_zsh_input_mark
            fi
            return 0
        }

        __w11term_zsh_preexec() {
            if [[ ${(t)PS1} != *readonly* ]]; then
                PS1=${PS1%"$__w11term_zsh_input_mark"}
            fi
            __w11term_zsh_running=1
            builtin printf '\033]133;C\007'
        }

        autoload -Uz add-zsh-hook
        if add-zsh-hook precmd __w11term_zsh_prompt &&
           add-zsh-hook preexec __w11term_zsh_preexec; then
            precmd_functions=(__w11term_zsh_finish "${precmd_functions[@]}")
            builtin printf '%s\n' 'w11term command recording is enabled for this Zsh session.'
        else
            add-zsh-hook -d precmd __w11term_zsh_prompt
            add-zsh-hook -d preexec __w11term_zsh_preexec
            builtin printf '%s\n' 'Could not add the w11term Zsh command hooks.' >&2
        fi
    }
    __w11term_enable_zsh_integration
    unfunction __w11term_enable_zsh_integration
fi
