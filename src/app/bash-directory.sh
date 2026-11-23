# Report the working directory to the terminal before each Bash prompt.
if [ -z "${BASH_VERSION-}" ]; then
    printf '%s\n' 'Paste this setup into an interactive Bash shell.' >&2
elif [[ $- != *i* ]]; then
    : # Bash can read startup files while running a noninteractive SSH command.
elif (( BASH_VERSINFO[0] < 5 || (BASH_VERSINFO[0] == 5 && BASH_VERSINFO[1] < 1) )); then
    builtin printf '%s\n' 'The w11term directory hook requires Bash 5.1 or later.' >&2
else
    __w11term_enable_bash_directory() {
        local declaration flags hook
        declaration=$(builtin declare -p PROMPT_COMMAND 2>/dev/null) || declaration=
        flags=${declaration#declare -}
        flags=${flags%% *}
        case $flags in
            ''|-|a|ax|x) ;;
            *)
                builtin printf '%s\n' 'The w11term directory hook needs a writable Bash PROMPT_COMMAND.' >&2
                return 0
                ;;
        esac
        for hook in "${PROMPT_COMMAND[@]}"; do
            [[ $hook == __w11term_report_directory ]] && return 0
        done

        __w11term_report_directory() {
            local LC_ALL=C directory=${PWD-} encoded= byte i
            if [[ $directory != /* ]]; then
                builtin printf '\033]7;file://\007'
                return 0
            fi
            # OSC 7 takes a file URI. Encode path bytes without running other programs at the prompt.
            for ((i = 0; i < ${#directory}; i++)); do
                byte=${directory:i:1}
                case $byte in
                    [a-zA-Z0-9._~/-]) ;;
                    *) builtin printf -v byte '%%%02X' "'$byte" ;;
                esac
                encoded+=$byte
            done
            builtin printf '\033]7;file://%s\007' "$encoded"
        }
        PROMPT_COMMAND+=(__w11term_report_directory)
        builtin printf '%s\n' 'w11term directory reporting is enabled for this Bash session.'
    }
    __w11term_enable_bash_directory
    unset -f __w11term_enable_bash_directory
fi
