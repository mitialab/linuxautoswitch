# linuxautoswitch

**Language:** English | [Русский](README.ru.md)

> **🤖 This project is vibecoded.** Essentially all of the code, the design
> decisions, the tests, and this README were written by
> [Claude](https://claude.ai) (Anthropic's AI, via Claude Code) from
> natural-language prompts - not typed by hand. The repository owner
> directed it at a high level and reviewed the results, but did not author
> the implementation line by line. This daemon reads every raw keystroke
> from your keyboard and injects synthetic key events back into your
> session - please actually read the code yourself (it's not that long)
> before you trust it with that, rather than taking a README's word for it.

Automatic EN/RU keyboard layout correction for [Omarchy](https://omarchy.org)
(Hyprland). Inspired by [Caramba Switcher](https://caramba.io) and the
classic Windows tool Punto Switcher: type a word in the wrong layout -
`ghbdtn` instead of `привет` - and it's detected and fixed automatically:
the wrong text is deleted, your keyboard layout is switched, and the correct
word is retyped.

> Built on the word lists from [dwyl/english-words](https://github.com/dwyl/english-words)
> and [danakt/russian-words](https://github.com/danakt/russian-words), and on
> [`wtype`](https://github.com/atx/wtype) for typing corrections back into
> Wayland apps. Full credits at the bottom of this file.

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
4. Automatic detection only fires when it can find the word in one
   dictionary but not the other - names, jargon, and anything not in either
   dictionary go untouched. For those, a manual hotkey (default:
   double-tap Left Shift) flips the current or last word to the other
   language regardless of dictionary validity, the same way Caramba's own
   manual-fix shortcut does. See [Manual correction & pausing](#manual-correction--pausing).

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
- [`wtype`](https://github.com/atx/wtype)
- Rust toolchain (to build)
- Your user in the `input` group, to read raw keyboard events (the installer
  handles this for you)

## Install

1. **Install the two system dependencies** (Omarchy is Arch-based, so
   `pacman` works out of the box):

   ```sh
   sudo pacman -S wtype rust
   ```

2. **Clone this repository** and enter it:

   ```sh
   git clone https://github.com/mitialab/linuxautoswitch
   cd linuxautoswitch
   ```

3. **Run the installer:**

   ```sh
   ./install.sh
   ```

   This builds a release binary, installs it to `~/.local/bin`, installs a
   `systemd --user` unit, writes a default config to
   `~/.config/linuxautoswitch/config.toml`, and enables and starts the
   service.

4. **If you weren't already in the `input` group**, the installer adds you
   to it and tells you to do so - this needs a fresh login to take effect:

   ```sh
   # log out and back in (or reboot), then:
   cd linuxautoswitch && ./install.sh
   ```

5. **Check that it's running:**

   ```sh
   systemctl --user status linuxautoswitch
   journalctl --user -u linuxautoswitch -f
   ```

   Try typing a word in the wrong layout (e.g. type `ghbdtn` with an
   English layout active) - it should turn into `привет` automatically.

### Manual start (no systemd)

If your session doesn't propagate environment variables into
`systemd --user` (some non-UWSM Hyprland setups don't), add this to your
Hyprland config instead of using the systemd service:

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

## Manual correction & pausing

Automatic detection needs a word to be recognizable in one dictionary but
not the other - it won't touch a name, a piece of jargon, or anything that
just isn't in either dictionary. Two hotkeys, both reconfigurable in
`[hotkeys]` in the config, cover what automatic detection can't:

- **Double-tap Left Shift** (`hotkeys.manual_correct_key`,
  `hotkeys.manual_correct_window_ms`) - flips the word under the cursor to
  the other language, no dictionary check at all. Works on the word you're
  still typing, or (if you've already moved on) the last completed one;
  press it again to flip back, same as Caramba's own manual-fix shortcut.
- **Left Shift + Right Shift together** (`hotkeys.toggle_pause_keys`) -
  pauses or resumes the whole daemon. Always works, even while already
  paused or focused on an excluded window, since it's the way out of both.

Set `hotkeys.enabled = false` to turn off both, or change either key
combination to any of: `leftshift`, `rightshift`, `leftctrl`, `rightctrl`,
`leftalt`, `rightalt`, `leftmeta`/`leftsuper`, `rightmeta`/`rightsuper`.

## Checking status from your shell

The running daemon exposes its state over a small control socket, and the
same binary talks to it as a client when run with a subcommand:

```sh
linuxautoswitch status          # "linuxautoswitch: running - layout EN"
linuxautoswitch status --json   # {"paused":false,"lang":"en","excluded":false}
linuxautoswitch pause           # pause (same effect as the pause hotkey)
linuxautoswitch resume
linuxautoswitch toggle
```

For a status-bar indicator (Omarchy's default bar is
[Waybar](https://github.com/Alexays/Waybar)) showing the current layout as
text - "EN"/"RU", or "⏸ EN" while paused - add a custom module:

```jsonc
// ~/.config/waybar/config
"custom/linuxautoswitch": {
  "exec": "linuxautoswitch waybar",
  "interval": 2,
  "return-type": "json"
}
```

and reference `"custom/linuxautoswitch"` in one of the bar's module lists.

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
- The manual-flip hotkey has the same "acting after the fact" caveat as
  automatic correction: if you've already pressed Enter and moved on (a
  message sent, a new empty line focused), flipping the "last word"
  retypes it wherever the cursor happens to be now, not where it actually
  is on screen.
- Only one daemon instance can hold the control socket (used by
  `status`/`pause`/`resume`/`toggle`/`waybar`) at a time; a second instance
  still runs, but logs a warning and skips starting its own socket.

## Uninstall

```sh
systemctl --user disable --now linuxautoswitch
rm ~/.local/bin/linuxautoswitch ~/.config/systemd/user/linuxautoswitch.service
systemctl --user daemon-reload
```

## Credits

This project wouldn't work, or exist, without:

- **[Claude](https://claude.ai) / Claude Code (Anthropic)** - wrote the code,
  the tests, and this documentation. See the disclosure at the top of this
  file.
- **Concept**: [Caramba Switcher](https://caramba.io) (macOS/Windows) and
  the original Punto Switcher, whose "type it wrong, get it fixed
  automatically" approach this project ports to Linux/Hyprland.
- **Dictionaries**: [dwyl/english-words](https://github.com/dwyl/english-words)
  (~370k words, Unlicense/public domain) and
  [danakt/russian-words](https://github.com/danakt/russian-words) (~1.5M word
  forms, MIT license) - embedded directly as the wrong-layout detection data.
- **[`wtype`](https://github.com/atx/wtype)** - does the actual work of
  retyping corrections into Wayland apps, via the
  `virtual-keyboard-unstable-v1` protocol.
- **[Hyprland](https://hyprland.org)** - the compositor this is built for,
  and whose IPC (`hyprctl`, the event socket) drives window/layout
  detection and switching.
- **Rust crates**: [`evdev`](https://crates.io/crates/evdev) for raw
  keyboard input, [`fst`](https://crates.io/crates/fst) (BurntSushi) for the
  compact embedded dictionaries, plus `clap`, `serde`, `toml`, `tracing`,
  and `anyhow`.

## License

linuxautoswitch's own code is [MIT-licensed](LICENSE). The two embedded
dictionaries and `wtype` carry their own licenses (also MIT, plus the
Unlicense for the English word list) - see
[THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md) for the full texts.
