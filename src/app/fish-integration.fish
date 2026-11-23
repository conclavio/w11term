if builtin status is-interactive; and not functions --query __w11term_fish_preexec
    # Fish 4 emits OSC 133 itself. Use hooks on older versions and when a
    # modern Fish explicitly reports that its mark-prompt feature is off.
    if builtin string match --quiet --regex '^[0-3]\.' -- "$version"; or begin
        builtin status test-feature mark-prompt
        test $status -eq 1
    end
        function __w11term_fish_prompt --on-event fish_prompt
            builtin printf '\033]133;A\007'
        end

        function __w11term_fish_preexec --on-event fish_preexec
            set -l encoded (builtin string escape --style=url -- "$argv[1]")
            # The URL encoding is ASCII. Count each %HH as one original byte.
            set -l size (builtin string length -- (builtin string replace --all --regex '%[[:xdigit:]]{2}' '%' -- "$encoded"))
            if test "$size" -le 4096
                builtin printf '\033]133;C;cmdline_url=%s\007' "$encoded"
            else
                builtin printf '\033]133;C\007'
            end
        end

        function __w11term_fish_postexec --on-event fish_postexec
            builtin printf '\033]133;D;%d\007' $status
        end

        builtin printf '%s\n' 'w11term command recording is enabled for this Fish session.'
    end
end
