# grogu

A standalone Rust binary that paints a coherent palette across the whole
desktop in one shot:

| target | what it writes |
| --- | --- |
| **[Noctalia shell](https://github.com/noctalia-dev/noctalia-shell)** | patches `colorSchemes.predefinedScheme` + `darkMode` in `~/.config/noctalia/settings.json`. The matching built-in scheme activates immediately. |
| **[niri](https://github.com/YaLTeR/niri)** | writes `~/.config/niri/grogu.kdl`, an include-able snippet with focus-ring colours. niri live-reloads. |
| **[telia](https://github.com/foolish-dev/telia)** | sets the `theme` row in telia's sqlite prefs store. telia picks it up on next launch. |
| **vim / neovim** | drops `~/.vim/colors/grogu.vim` and/or `~/.config/nvim/colors/grogu.vim`. Activate with `:colorscheme grogu`. |
| **[kitty](https://sw.kovidgoyal.net/kitty/)** | writes `~/.config/kitty/grogu.conf`. Activate by adding `include grogu.conf` to `kitty.conf` once; reload with `kill -SIGUSR1 $(pgrep kitty)` or kitty's default `Ctrl+Shift+F5`. |
| **[ghostty](https://ghostty.org)** | writes `~/.config/ghostty/themes/grogu`. Activate by adding `theme = grogu` to `~/.config/ghostty/config`; ghostty live-reloads on save. |

Three themes ship: `tokyo-night`, `catppuccin`, `dracula`. Or extract
a palette directly from the current wallpaper (see "v2: wallpaper
extraction" below).

```sh
cargo install --path .

grogu list                       # tokyo-night / catppuccin / dracula
grogu paths                      # everywhere grogu reads or writes
grogu apply                      # defaults to telia's stored theme pref
grogu apply --theme catppuccin
grogu apply --no-vim --dry-run   # see what would change without touching files
grogu apply --no-kitty --no-ghostty  # skip the terminals

# v2: derive the whole palette from the current wallpaper
grogu apply --extract                              # reads Noctalia's wallpaper cache
grogu apply --extract /path/to/wallpaper.jpg       # explicit path
grogu extract /path/to/wallpaper.jpg               # preview without writing
```

## v2: wallpaper extraction (pywal-style)

`grogu apply --extract` derives the full 16-colour palette from the
wallpaper itself instead of reading a predefined theme. The pipeline:

1. Load the image (PNG / JPEG / WebP / BMP via the `image` crate).
2. Resample to 256×256 for speed.
3. Convert to CIE Lab and run k-means clustering (k=12, three seeds,
   keep the lowest within-cluster variance).
4. Sort centroids by lightness; pin `bg` to the darkest cluster
   (clamped to L∈[6,16] so terminals stay readable on light wallpapers)
   and `fg` to the lightest (clamped to L∈[78,92]).
5. For each accent slot (red, yellow, green, cyan, blue, purple), pick
   the cluster closest to the canonical hue (25°, 80°, 130°, 180°,
   230°, 290°), then pull its chroma toward the target hue when the
   wallpaper doesn't have that colour — otherwise monochromatic
   wallpapers would collapse half the accents into one shade.

The extracted palette lands in every target:

- **Noctalia** — writes a full Material-Design + ANSI scheme JSON to
  `~/.config/noctalia/colorschemes/Grogu/Grogu.json` (the `find
  -mindepth 2` Noctalia uses to enumerate schemes requires that extra
  directory), sets `colorSchemes.predefinedScheme = "Grogu"`, and
  *also* writes the flat m-color map directly to
  `~/.config/noctalia/colors.json`. The direct write is what makes the
  bar repaint *now* — Noctalia's `schemes[]` is cached once at startup
  with no rescan IPC, so a fresh user scheme is invisible until the
  next restart, but its `Color.qml` watches `colors.json` and
  live-reloads every binding on change. On the next Noctalia restart,
  `predefinedScheme = "Grogu"` resolves to the scheme file written
  above and the palette persists.
- **niri**, **kitty**, **ghostty**, **vim/neovim** — same renderers
  as predefined mode, just parameterised over the extracted palette.
- **telia** — telia only ships three themes (`tokyo-night`,
  `catppuccin`, `dracula`) with no custom-palette support, so grogu
  picks the *nearest* predefined theme by squared-distance on
  `bg + purple` in sRGB and writes that name to telia's pref store.

Wallpaper source resolution order:

1. Explicit path: `grogu apply --extract /path/to/img.jpg`
2. `$GROGU_WALLPAPER` env var (intended for hook invocations)
3. Noctalia's wallpaper cache at `~/.cache/noctalia/wallpapers.json`
   — grogu walks the JSON looking for the first string that names an
   existing file, preferring `dark` keys.

Preview without writing:

```sh
grogu extract /path/to/img.jpg
```

prints the extracted palette so you can sanity-check what `apply
--extract` will do.

## Hook it to Noctalia's wallpaper rotation

Noctalia has a hooks system in its settings panel (Settings → Hooks).
Add a hook that runs after a wallpaper change:

```
event: wallpaper.changed
command: grogu apply --extract
```

(Drop `--extract` if you'd rather lock to a predefined theme.)
If Noctalia's hook system can pass the new wallpaper path as an
argument, set the command to `grogu apply --extract %{wallpaper}` —
otherwise grogu reads the path back out of Noctalia's wallpaper cache.

Now every wallpaper rotation re-paints the rest of the desktop. niri
picks up the new colours automatically (live reload), telia uses the
updated theme on next launch, and the next time you open vim,
`colorscheme grogu` reflects the new palette.

If you'd rather drive it from the compositor instead, bind it:

```hyprland
bind = SUPER SHIFT, T, exec, grogu apply
```

```kdl
# niri
binds {
    Mod+Shift+T { spawn "grogu" "apply"; }
}
```

## How the niri include works

niri's `include` directive merges `layout { ... }` properties across
files (per its docs) and appends `window-rule` blocks. So `grogu.kdl`
sets the focus-ring colours without clobbering the rest of your layout
config:

```kdl
# ~/.config/niri/config.kdl — add once
include "grogu.kdl"
```

Subsequent `grogu apply` runs rewrite `grogu.kdl` only; niri live-reloads.

## How the Noctalia patch works

grogu reads the existing `settings.json` as JSON, mutates only
`colorSchemes.useWallpaperColors` / `colorSchemes.predefinedScheme` /
`colorSchemes.darkMode`, and writes the rest back verbatim. Other
top-level keys (`bar`, `dock`, custom keys you added) are preserved.

Predefined mode (`--theme tokyo-night|catppuccin|dracula`): the
settings.json write alone doesn't repaint a running Noctalia —
`ColorSchemeService` only re-runs `applyScheme` on darkMode changes or
explicit IPC. After writing, grogu calls
`qs -c noctalia-shell ipc call colorScheme set <Name>` and
`darkMode setDark|setLight` to make the live shell repaint. If `qs`
isn't on PATH or noctalia isn't running, IPC silently no-ops and the
file write persists for the next launch.

Extracted mode (`--extract`): instead of nudging IPC (which would
error since `Grogu` isn't in the cached `schemes[]` until restart),
grogu writes `colors.json` directly. Noctalia's `Color.qml`
file-watches `colors.json` and live-reloads every m-color binding —
so the bar repaints with the wallpaper-derived palette immediately,
no restart needed.

## How the telia integration works

grogu opens telia's sqlite store at
`$XDG_DATA_HOME/telia/telia.sqlite` (or `~/.local/share/telia/telia.sqlite`)
and does an `INSERT OR REPLACE INTO prefs (key, value) VALUES ('theme', ?)`.
This is the same row telia's `/theme NAME` slash command writes — telia
reads it on every launch.

If telia hasn't run on the machine, grogu skips this target with a note.

## Terminal reload behaviour

- **ghostty** live-reloads its config on save, so a `grogu apply` repaints open windows automatically.
- **kitty** doesn't auto-detect config changes; either set up `Ctrl+Shift+F5` (kitty's default reload bind), enable `allow_remote_control yes` and call `kitty @ set-colors --all --configured ~/.config/kitty/grogu.conf`, or just `kill -SIGUSR1 $(pgrep kitty)`.

If you want grogu to ping kitty after applying, run:

```sh
grogu apply && pkill -SIGUSR1 kitty 2>/dev/null || true
```

(grogu doesn't do this itself — it keeps the binary side-effect free
beyond file writes.)

## Env overrides

- `NOCTALIA_CONFIG_DIR` / `NOCTALIA_SETTINGS_FILE` — point at a non-default Noctalia install
- `XDG_CONFIG_HOME` / `XDG_DATA_HOME` — standard XDG overrides for niri / telia paths

## License

MIT.
