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
# alpha < 1 makes the terminal background translucent. Combine with
# the compositor blur rule below + `/transparent on` inside telia and
# the wallpaper bleeds through with a Tokyo Night tint.
alpha=0.85
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
windowrulev2 = opacity 0.92 0.88 override, class:^(telia-float)$
windowrulev2 = bordersize 2,              class:^(telia-float)$
windowrulev2 = rounding 12,               class:^(telia-float)$
windowrulev2 = animation slide,           class:^(telia-float)$
# Blur the wallpaper behind the floating panel. Paired with `alpha`
# in foot.ini + `/transparent on` inside telia, this gives the
# frosted-glass look the SVGs are styled after.
windowrulev2 = noblur 0,                  class:^(telia-float)$
windowrulev2 = xray on,                   class:^(telia-float)$

# Tokyo Night borders that match the TUI accents
general {
    col.active_border   = rgba(bb9af7ff) rgba(7dcfffff) 45deg
    col.inactive_border = rgba(414868aa)
}

# Strong blur tuned for translucent terminals. Crank passes if your
# GPU can spare it; back off `size` for less smearing.
decoration {
    blur {
        enabled = true
        size = 8
        passes = 3
        new_optimizations = true
        ignore_opacity = true
        xray = true
    }
}
```

`SUPER+L` opens a 1240×760 floating telia, centred, ~90% opaque,
rounded corners, with a purple→cyan animated border that picks up the
same gradient as the SVG banner — the wallpaper behind it is blurred
by Hyprland so the translucent foot background reads as frosted glass.

## Niri

A drop-in config block lives at [`assets/niri-rice.kdl`](../assets/niri-rice.kdl) —
copy it into `~/.config/niri/config.kdl` or `include` it. What it does:

- **Dedicated `agent` workspace** — telia opens on its own named
  workspace so it never gets shuffled in with editor windows.
  `Mod+'` jumps to it from anywhere.
- **Idempotent launcher** — `Mod+L` runs `pgrep` first, so it
  either focuses the existing telia or spawns a new one. No
  duplicate windows on accidental double-press.
- **Spawn at startup** — `spawn-at-startup "foot" "--app-id=telia-float" "-e" "telia"`
  prewarms telia in the background so the first `Mod+L` is instant.
- **Tokyo Night focus ring** — purple `#bb9af7` when focused,
  dim `#414868` when not. Borders are off; the focus ring carries
  the accent.
- **Floating, centered, 1280 × 800** with rounded `12 12 12 12`
  corners that line up with the TUI's own rounded chat / input
  block corners.
- **90% opacity** so the wallpaper bleeds through. Niri doesn't ship
  window blur as of writing — for a frosted-glass look run a blurred
  wallpaper (e.g. `swww img --transition-type=none blurred.png`) and
  pair it with `/transparent on` inside telia.

```kdl
// minimal — the full version is in assets/niri-rice.kdl

binds {
    Mod+L {
        spawn "sh" "-c"
            "pgrep -f 'foot.*telia-float' >/dev/null || foot --app-id=telia-float -e telia";
    }
    Mod+Apostrophe { focus-workspace "agent"; }
}

workspace "agent" {}

window-rule {
    match app-id="telia-float"
    open-on-workspace "agent"
    open-floating true
    default-column-width { fixed 1280; }
    default-window-height { fixed 800; }
    focus-ring {
        active-color  "#bb9af7"
        inactive-color "#414868"
        width 2
    }
    border { off }
    geometry-corner-radius 12 12 12 12
    opacity 0.90
}

spawn-at-startup "foot" "--app-id=telia-float" "-e" "telia"
```

Niri's scrolling layout pairs nicely with the chat-log paradigm —
the workspace scrolls horizontally past the floating telia panel
just like the chat scrolls vertically past prior turns.

## Wallpaper

`hyprpaper` (Hyprland) or `swww` (Niri) loaded with any Tokyo Night
wallpaper rounds out the palette. The colour-palette file shipped
with [tokyo-night-gtk](https://github.com/Fausto-Korpsvart/Tokyo-Night-GTK-Theme)
or the wallpapers from the
[tokyo-night-vscode-theme](https://github.com/enkia/tokyo-night-vscode-theme)
repo work well.

## Blur & transparency

The frosted-glass look is a three-layer stack — each piece is opt-in,
and the whole effect only lands when all three are on:

1. **Terminal alpha** — `alpha=0.85` in `foot.ini` (or the equivalent
   in alacritty / kitty / wezterm). Makes the terminal's background
   cell translucent.
2. **Compositor blur** — Hyprland's `decoration { blur { enabled = true } }`
   blurs whatever sits behind the translucent terminal. Niri has no
   native window blur as of writing; pre-blur the wallpaper instead.
3. **TUI transparent mode** — `/transparent on` inside telia (or
   `:transparent on`, or set `TELIA_TRANSPARENT=1` in your shell).
   This swaps every `bg(theme.bg)` paint for `Color::Reset`, so the
   terminal alpha actually shows through instead of being masked by
   the TUI's own opaque Tokyo Night background. The preference
   persists across sessions via the sqlite store.

Selection highlight, status chips, and mode badges keep their solid
colours so they stay readable on top of the wallpaper.

## Result

You get a floating, centred τέλεια panel that opens with one keystroke,
sits on a Tokyo Night desktop, runs JetBrains Mono Nerd Font, and
matches the TUI's purple-cyan-grey accents end to end. With
`/transparent on` and the compositor blur, the panel reads as frosted
glass over the wallpaper while the chat text, selection highlight,
and status chips remain fully legible.
