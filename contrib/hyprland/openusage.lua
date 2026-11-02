-- Hyprland 0.55+ (Lua config): float the terminal used for `openusage add` and the Waybar right-click.
-- Require this file from hyprland.lua, or paste the rule in. The window title has to be "openusage":
--   xdg-terminal-exec --title=openusage openusage add claude
hl.window_rule({
  match = { title = "^openusage$" },
  float = true,
  center = true,
  size = { 720, 480 },
})
