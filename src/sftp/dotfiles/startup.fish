# >>> w11term dotfiles >>>
begin
    set -l _w11term_dotfiles_store "$HOME/.config/w11term/dotfiles"
    set -l _w11term_dotfiles_version
    if builtin test -r "$_w11term_dotfiles_store/fish-current"
        and builtin read --line _w11term_dotfiles_version < "$_w11term_dotfiles_store/fish-current"
        and builtin string match -rq '^[0-9a-f]{64}$' -- "$_w11term_dotfiles_version"
        set -l _w11term_dotfiles_dir "$_w11term_dotfiles_store/fish-$_w11term_dotfiles_version"
        if builtin test -r "$_w11term_dotfiles_dir/environment.fish"
            builtin source "$_w11term_dotfiles_dir/environment.fish"
        end
        if builtin status --is-interactive; and builtin test -r "$_w11term_dotfiles_dir/prompt.fish"
            builtin source "$_w11term_dotfiles_dir/prompt.fish"
        end
    end
end
# <<< w11term dotfiles <<<
