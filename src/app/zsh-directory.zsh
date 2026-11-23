if [ -z "${ZSH_VERSION-}" ]; then
    printf '%s\n' 'Paste this setup into an interactive Zsh shell.' >&2
elif [[ -o interactive ]]; then
    __w11term_enable_zsh_directory() {
        emulate -L zsh
        if (( ${precmd_functions[(I)__w11term_report_zsh_directory]} )); then
            return 0
        fi
        if [[ ${(t)precmd_functions} == *readonly* ]]; then
            builtin printf '%s\n' 'The w11term directory hook needs writable Zsh prompt hooks.' >&2
            return 0
        fi

        __w11term_report_zsh_directory() {
            emulate -L zsh
            unsetopt multibyte
            local LC_ALL=C directory=${PWD-} encoded= byte i
            if [[ $directory != /* ]]; then
                builtin printf '\033]7;file://\007'
                return 0
            fi
            for ((i = 1; i <= ${#directory}; i++)); do
                byte=${directory[i]}
                case $byte in
                    [a-zA-Z0-9._~/-]) ;;
                    *) builtin printf -v byte '%%%02X' "'$byte" ;;
                esac
                encoded+=$byte
            done
            builtin printf '\033]7;file://%s\007' "$encoded"
        }

        autoload -Uz add-zsh-hook
        if add-zsh-hook precmd __w11term_report_zsh_directory; then
            builtin printf '%s\n' 'w11term directory reporting is enabled for this Zsh session.'
        else
            builtin printf '%s\n' 'Could not add the w11term Zsh prompt hook.' >&2
        fi
    }
    __w11term_enable_zsh_directory
    unfunction __w11term_enable_zsh_directory
fi
