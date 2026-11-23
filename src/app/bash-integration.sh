# Enable command marks in the same interactive Bash versions as the directory setup.
if [ -n "${BASH_VERSION-}" ] && [[ $- == *i* ]] &&
   (( BASH_VERSINFO[0] > 5 || (BASH_VERSINFO[0] == 5 && BASH_VERSINFO[1] >= 1) )); then
    __w11term_enable_bash_integration() {
        local hook name declaration flags
        for hook in "${PROMPT_COMMAND[@]}"; do
            [[ $hook == __w11term_bash_prompt ]] && return 0
        done
        if ! builtin shopt -q promptvars; then
            builtin printf '%s\n' 'Bash command recording requires the promptvars option.' >&2
            return 0
        fi

        for name in PROMPT_COMMAND PS0 PS1; do
            declaration=$(builtin declare -p "$name" 2>/dev/null) || continue
            flags=${declaration#declare -}
            flags=${flags%% *}
            case $name:$flags in
                PROMPT_COMMAND:-|PROMPT_COMMAND:a|PROMPT_COMMAND:ax|PROMPT_COMMAND:x|PS[01]:-|PS[01]:x) ;;
                *)
                    builtin printf '%s\n' 'Bash command recording needs writable prompts and PROMPT_COMMAND.' >&2
                    return 0
                    ;;
            esac
        done
        if [[ ${PS0-}${PS1-} == *']133;'* || ${PS0-}${PS1-} == *']633;'* ]]; then
            builtin printf '%s\n' 'The Bash prompt already supplies semantic markers.' >&2
            return 0
        fi

        __w11term_bash_running=0
        __w11term_bash_output_mark=$'\033]133;C\007'
        # PS0 is expanded for each accepted command. The substring offset
        # arms the finish hook in the current shell while the value prints C.
        __w11term_bash_ps0_suffix='${__w11term_bash_output_mark:__w11term_bash_running=1,0}'
        __w11term_bash_ps1_suffix='\['$'\033]133;B\007''\]'

        __w11term_bash_finish() {
            local exit_code=$?
            if (( __w11term_bash_running )); then
                __w11term_bash_running=0
                builtin printf '\033]133;D;%d\007' "$exit_code"
            fi
            return 0
        }

        __w11term_bash_prompt() {
            local output_prompt=${PS0-} input_prompt=${PS1-}
            # A user hook may change the prompt, so decorate its latest value.
            output_prompt=${output_prompt//"$__w11term_bash_ps0_suffix"/}
            input_prompt=${input_prompt//"$__w11term_bash_ps1_suffix"/}
            if [[ -v PS0 && ${PS0@a} == *r* ]] || [[ -v PS1 && ${PS1@a} == *r* ]]; then
                return 0
            fi
            if builtin shopt -q promptvars; then
                PS0=$output_prompt$__w11term_bash_ps0_suffix
                PS1=$input_prompt$__w11term_bash_ps1_suffix
                builtin printf '\033]133;A\007'
            else
                PS0=$output_prompt
                PS1=$input_prompt
            fi
        }

        # Bash passes the original status separately to each array entry.
        if [[ -v 'PROMPT_COMMAND[0]' ]]; then
            PROMPT_COMMAND=(__w11term_bash_finish "${PROMPT_COMMAND[@]}")
        else
            PROMPT_COMMAND[0]=__w11term_bash_finish
        fi
        PROMPT_COMMAND+=(__w11term_bash_prompt)
        builtin printf '%s\n' 'w11term command recording is enabled for this Bash session.'
    }
    __w11term_enable_bash_integration
    unset -f __w11term_enable_bash_integration
fi
