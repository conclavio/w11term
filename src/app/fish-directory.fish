if status is-interactive; and not functions --query __w11term_report_fish_directory
    function __w11term_report_fish_directory --on-event fish_prompt
        # Fish can register its stock OSC 7 reporter during the first prompt.
        # Use one directory reporter so a hostname cannot replace this session's report.
        functions --erase __fish_update_cwd_osc __update_cwd_osc
        if string match --quiet '/*' -- "$PWD"
            builtin printf '\033]7;file://%s\007' (builtin string escape --style=url -- "$PWD")
        else
            builtin printf '\033]7;file://\007'
        end
    end

    __w11term_report_fish_directory
    builtin printf '%s\n' 'w11term directory reporting is enabled for this Fish session.'
end
