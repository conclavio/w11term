" Called only by the isolated zmx PTY regression.
set noswapfile nobackup noundofile noloadplugins
set shortmess+=I noshowcmd noruler notitle noicon
set ttimeoutlen=50
function! W11Effects()
  call writefile(["\x07\x1b]52;c;Zml4dHVyZQ==\x1b\\\x1b]9;w11-notification\x1b\\\x1b]440;1;play;shot\x1b\\"], '/dev/tty', 'b')
endfunction
function! W11Report()
  let report = {'pid': getpid(), 'line': getline(1), 'rows': &lines, 'cols': &columns, 'modified': &modified}
  call writefile([json_encode(report)], $W11_REPORT . '.tmp')
  call rename($W11_REPORT . '.tmp', $W11_REPORT)
endfunction
autocmd VimEnter * call W11Report()
nnoremap <silent> gQ :call W11Effects()<CR>
nnoremap <silent> gR :call W11Report()<Bar>call writefile(["\x1b]2;w11-final-report\x1b\\"], '/dev/tty', 'b')<CR>
