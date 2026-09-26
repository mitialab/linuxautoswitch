# linuxautoswitch

Automatic EN/RU keyboard layout correction for [Omarchy](https://omarchy.org)
(Hyprland). Inspired by [Caramba
Switcher](https://caramba.io) / the classic Windows tool Punto Switcher: type
a word in the wrong layout - `ghbdtn` instead of `привет` - and it's detected
and fixed automatically: the wrong text is deleted, your keyboard layout is
switched, and the correct word is retyped.

## How it works

1. A background daemon reads raw keystrokes from your keyboard's
   `/dev/input/eventN` device. Evdev key codes identify a *physical* key
   position, independent of whatever XKB layout is currently active - so the
   same key sequence can be reconstructed as either its English or its
   Russian interpretation, using the standard QWERTY/ЙЦУКЕН physical mapping,
   without ever needing to know what layout was actually active while typing.
2. After *every* keystroke that extends a word, it checks: is what actually
   appeared on screen a real word in the currently active language? If not,
   is the *other* interpretation a real word? Checking on every keystroke,
   rather than only once a word ends, is what lets it catch a browser
   address bar or a chat message: by the time you press Enter, the word is
   usually already fixed, instead of the correction arriving after the
   browser has navigated or the message has sent. Pressing Space after a
   word runs one more check as a backstop; other word endings (Enter, Tab,
   punctuation, arrows) just reset the word-tracking state without
   rechecking it, since the eager check already had every chance to catch
   it and by then the app has typically already acted on that key anyway.
3. If so: delete what was typed (plus the space, if a space triggered the
   check), switch the active layout via Hyprland's
   `switchxkblayout`, and retype the corrected word - via Wayland's virtual
   keyboard protocol (`wtype`), so it never sees its own corrections as new
   input.

Dictionary lookups run against ~370k English and ~1.5M Russian word forms
(plus a curated list of common site/brand/tech names like "google" or
"github" that wouldn't otherwise show up in a plain word dictionary),
compiled at build time into compact `fst` sets and embedded in the binary
(a few MB total, no per-word heap allocation at runtime - this is meant to
run in the background indefinitely).

Steam games are skipped automatically: Steam sets a `SteamAppId` environment
variable on every game it launches (native or Proton), which is checked
instead of trying to enumerate every game's window class.

## Requirements

- [Omarchy](https://omarchy.org) or any Hyprland-based Wayland session
- [`wtype`](https://github.com/atx/wtype) (`sudo pacman -S wtype`)
- Your user must be in the `input` group to read raw keyboard events
  (`install.sh` handles this)
- Rust toolchain to build (`sudo pacman -S rust`)

## Install

```sh
git clone https://github.com/mitialab/linuxautoswitch
cd linuxautoswitch
./install.sh
```

This builds a release binary, installs it to `~/.local/bin`, installs a
`systemd --user` unit, writes a default config to
`~/.config/linuxautoswitch/config.toml`, and starts the service.

If you're not already in the `input` group, the script adds you to it - log
out and back in, then run it again (or just start the service once you're
back).

```sh
systemctl --user status linuxautoswitch
journalctl --user -u linuxautoswitch -f
```

### Manual start (no systemd)

If your session doesn't propagate environment variables into
`systemd --user` (some non-UWSM Hyprland setups don't), add this to your
Hyprland config instead:

```
exec-once = /home/you/.local/bin/linuxautoswitch
```

## Configuration

See [`assets/config.example.toml`](assets/config.example.toml) for the full
reference, copied to `~/.config/linuxautoswitch/config.toml` on install.
Key settings:

- `general.excluded_classes` - window classes to never touch (terminals,
  password managers, and Steam by default - find a class with
  `hyprctl activewindow`; a trailing `*` matches as a prefix)
- `general.exclude_steam_games` - skip Steam games (on by default, detected
  via the `SteamAppId` env var rather than window class)
- `general.eager_correction` - correct mid-word, on every keystroke, instead
  of waiting for space/enter (on by default; see limitations below)
- `general.min_word_length` / `general.eager_min_word_length` - two separate
  thresholds on purpose: a word confirmed complete by pressing Space can be
  trusted even if short (`min_word_length`, default 2 - catches "да", "но"),
  while a still-growing buffer checked mid-word needs a higher bar
  (`eager_min_word_length`, default 4) since a short one might just be the
  first letters of something longer. Single-letter words are never
  corrected, regardless of either setting.
- `layouts.english_index` / `russian_index` - must match the order of
  `kb_layout` in your Hyprland config (e.g. `kb_layout = us,ru` means
  `english_index = 0`, `russian_index = 1`)
- `hypr.device_name_override` - force a specific keyboard device on
  multi-keyboard setups (`hyprctl devices | grep -A2 Keyboard`)

## Known limitations

- Only two layouts (English/Russian) are supported for now.
- Hyphenated words, apostrophe-contractions and anything with digits inside
  a "word" aren't corrected - those keys are treated as word boundaries.
- Multi-keyboard setups may need `hypr.device_name_override` set manually if
  auto-detection guesses the wrong device.
- No per-application layout memory (Caramba's other headline feature) - this
  focuses purely on wrong-layout typo correction. Could be added later.
- With `eager_correction` on, a correction can very occasionally fire a beat
  early, mid-word: if a partial word you're still typing happens to
  coincidentally spell a complete real word in the other language, it gets
  corrected right then rather than once you finish typing. In practice this
  is self-correcting (the layout gets switched either way, so the rest of
  the word still comes out right) unless the coincidence happens to be a
  real word in the *other* language while you're still mid-word in the
  *currently active* one - rare, but possible with a 1.5M-entry Russian
  dictionary. Raise `eager_min_word_length`, or turn `eager_correction` off
  to fall back to boundary-only correction, if this happens often enough to
  bother you.

## Uninstall

```sh
systemctl --user disable --now linuxautoswitch
rm ~/.local/bin/linuxautoswitch ~/.config/systemd/user/linuxautoswitch.service
systemctl --user daemon-reload
```

## License

Dictionary sources: [dwyl/english-words](https://github.com/dwyl/english-words)
(Unlicense) and [danakt/russian-words](https://github.com/danakt/russian-words)
(MIT).
