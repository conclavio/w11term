# >>> w11term tool PATH @ID@ >>>
if builtin string match -rq -- '^/[^:]*$' "$HOME"
    set -l _w11term_tool_bin (builtin string replace -r -- '/*$' '/@BIN@' "$HOME")
    if not builtin contains -- "$_w11term_tool_bin" $PATH
        builtin set -gx PATH $PATH "$_w11term_tool_bin"
    end
end
# <<< w11term tool PATH @ID@ <<<
