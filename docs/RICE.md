# Rice — tiling / scrolling Wayland compositors

τέλεια lives inside a terminal emulator, so the "rice" is really about
*how the compositor frames that terminal*. These are drop-in starter
configs for [Hyprland](https://hypr.land) and
[niri](https://github.com/YaLTeR/niri) that float a τέλεια session in
its own window, key-bound, with the Tokyo Night palette running through
the terminal, the font, and the wallpaper.

The pieces:

- terminal emulator running JetBrains Mono Nerd Font + a Tokyo Night
  palette → matches the SVGs and the new TUI accents
- compositor keybind that spawns the terminal with a recognisable
  `app-id` → so the floating rule can target it
- floating + sized window rule pinned around the centre → telia is a
  full-screen-feel TUI but lives inside a small floating panel here

## Terminal — foot (or alacritty / kitty)

The TUI inherits whatever font + colours the host terminal is using.
Pick the one your compositor ships with and point it at JetBrains Mono
Nerd Font.

`~/.config/foot/foot.ini`:

```ini
font=JetBrainsMonoNerdFont-Regular:size=12
pad=12x12
dpi-aware=yes

[colors]
background=1a1b26
foreground=a8a8b2
regular0=15161e
regular1=f7768e
regular2=9ece6a
regular3=e0af68
regular4=7aa2f7
regular5=bb9af7
regular6=7dcfff
regular7=a9b1d6
bright0=414868
bright1=f7768e
bright2=9ece6a
bright3=e0af68
bright4=7aa2f7
bright5=bb9af7
bright6=7dcfff
bright7=c0caf5
```

(Alacritty / kitty equivalents land the same RGB values in their own
syntax — the palette is Tokyo Night, lifted from the values in
`crates/telia-cli/src/tui.rs`.)

## Hyprland

`~/.config/hypr/hyprland.conf`:

```hyprland
# τέλεια — float in the centre, bound to SUPER+L
bind = SUPER, L, exec, foot --app-id=telia-float -e telia

windowrulev2 = float,                     class:^(telia-float)$
windowrulev2 = size 1240 760,             class:^(telia-float)$
windowrulev2 = center,                    class:^(telia-float)$
windowrulev2 = opacity 0.96 0.92 override, class:^(telia-float)$
windowrulev2 = bordersize 2,              class:^(telia-float)$
windowrulev2 = rounding 12,               class:^(telia-float)$
windowrulev2 = animation slide,           class:^(telia-float)$

# Tokyo Night borders that match the TUI accents
general {
    col.active_border   = rgba(bb9af7ff) rgba(7dcfffff) 45deg
    col.inactive_border = rgba(414868aa)
}
```

`SUPER+L` opens a 1240×760 floating telia, centred, ~95% opaque,
rounded corners, with a purple→cyan animated border that picks up the
same gradient as the SVG banner.

## Niri

`~/.config/niri/config.kdl`:

```kdl
binds {
    Mod+L { spawn "foot" "--app-id=telia-float" "-e" "telia"; }
}

window-rule {
    match app-id="telia-float"
    open-floating true
    default-column-width { fixed 1240; }
    default-window-height { fixed 760; }
    border {
        active-color "#bb9af7"
        inactive-color "#414868"
        width 2
    }
    geometry-corner-radius 12 12 12 12
    opacity 0.96
}

layout {
    border {
        width 2
        active-color "#bb9af7"
        inactive-color "#414868"
    }
}
```

Niri's scrolling layout pairs nicely with the chat-log paradigm — you
can scroll the workspace horizontally past telia just like you scroll
the chat itself.

## Wallpaper

`hyprpaper` (Hyprland) or `swww` (Niri) loaded with any Tokyo Night
wallpaper rounds out the palette. The colour-palette file shipped
with [tokyo-night-gtk](https://github.com/Fausto-Korpsvart/Tokyo-Night-GTK-Theme)
or the wallpapers from the
[tokyo-night-vscode-theme](https://github.com/enkia/tokyo-night-vscode-theme)
repo work well.

## Result

You get a floating, centred τέλεια panel that opens with one keystroke,
sits on a Tokyo Night desktop, runs JetBrains Mono Nerd Font, and
matches the TUI's purple-cyan-grey accents end to end. The TUI's own
rounded borders + the compositor's matching rounded corners stack
cleanly.
