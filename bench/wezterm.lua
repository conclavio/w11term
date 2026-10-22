local wezterm = require 'wezterm'
return {
  font = wezterm.font('Cascadia Mono'),
  font_size = 12.0, -- points: matches w11term's 16 device-independent pixels
  initial_cols = 110,
  initial_rows = 36,
  colors = { foreground = '#dbe0e8', background = '#0e1014' },
  default_cursor_style = 'SteadyBar',
  window_background_opacity = 1.0,
  window_padding = { left = 12, right = 12, top = 12, bottom = 12 },
  enable_tab_bar = false,
  check_for_updates = false,
  automatically_reload_config = false,
  ssh_domains = {},
  unix_domains = {},
}
