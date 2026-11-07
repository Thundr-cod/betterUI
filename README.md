# betterUI

A lightweight, native desktop shell for Windows 11: a top bar and a dock that
replace the Windows taskbar. Written in Rust against the Win32 API directly,
with no web view, so it starts instantly and uses a few MB of RAM.

## Status

| Part | What | State |
| --- | --- | --- |
| 1 | Top bar, taskbar replacement, RAM + network readout, clock | done |
| 2 | Audio flyout: devices (output / input) + per-app mixer | done |
| 3 | Bluetooth + control center (Wi-Fi, Bluetooth, brightness, volume) | done |
| 4 | Dock (auto-hides; also hides when a window covers it) | done |
| 5 | System tray dropdown | done |

## Getting it

Download `betterui.exe` from the newest release on the
[Releases page](https://github.com/Thundr-cod/betterUI/releases) and run it.
No installer and no other software needed. See the
[Code signing policy](#code-signing-policy).

## Versions

Every pull request raises the version in `Cargo.toml` (CI checks this), and
merging it publishes a release tagged `v<version>`. Releases before `v0.1.0`
are pre-releases. `v0.1.0` marks the MVP once it has been tested on a real PC.

Numbering rules (the last number must always count up):

- **Features** take the next version: `v0.0.5`.
- **Bug fixes** add two digits to the feature they fix: `v0.0.501`, `v0.0.502`, …
- **The next feature after any bug fix** moves to the next hundred: `v0.0.600`
  (its fixes are `v0.0.601`, …), because `v0.0.6` would count lower than `v0.0.502`.

| Release | Contents |
| --- | --- |
| v0.0.1 | Top bar, taskbar replacement, opacity/blur, audio flyout |
| v0.0.2 | Bluetooth dropdown + control center |
| v0.0.3 | Dock |
| v0.0.4 | Docs: future work plan |
| v0.0.5 | System tray dropdown (MVP feature-complete) |
| v0.1.0 | MVP, after testing on a real PC (planned) |

v0.0.1 was never published on its own (its PR merged together with v0.0.2's), so v0.0.2 is the first release.

## Future work

### Known issues from real-PC testing

Found while testing v0.0.801 on a real Windows 11 PC and not fixed yet.

| Issue | Notes |
| --- | --- |
| **Some tray icons never appear (e.g. the NVIDIA app)** | Steam and Claude now show up, but the NVIDIA app and other icons Windows keeps in its hidden-icons area don't. `tray_debug.txt` shows no message at all from any NVIDIA process, not even after betterUI asks every app to re-send its icons, so betterUI isn't filtering it out: the app never talks to betterUI's tray. Likely causes to check: the app ignores the `TaskbarCreated` broadcast, or it runs elevated, which stops Windows delivering that broadcast to it. Possible fixes: read the icons Explorer already holds, or re-ask only those apps a different way. |
| **Hide redundant tray icons** | Explorer's own Bluetooth icon duplicates betterUI's Bluetooth button. Decide which Explorer-owned icons to hide once the full list shows up. |
| **Dock divider looks low-res and uneven** | Better since v0.0.702 but still soft, and its thickness varies. Snap it to whole device pixels at the monitor's DPI so it's one crisp, even line. |
| **Code signing** | The SignPath Foundation application was not approved, so builds stay unsigned and Smart App Control blocks them. Pick another way to sign the exe. |

**Not yet tested on real hardware:** multiple monitors, brightness on DDC/CI
monitors, and tray click forwarding (clicking an icon in betterUI's tray
dropdown passes the click on to its app, e.g. right-click Steam for its menu).

### New features

These are planned for after `v0.1.0` and are **not** part of the MVP.

| Feature | Notes |
| --- | --- |
| **Dark or custom-colour flyouts** | The flyouts (audio, Bluetooth, control center, tray) are drawn white today. Give them their own colour setting, or a dark mode with the same tint, opacity and blur as the top bar and dock, so the whole shell matches. |
| **Start button on the top bar** | A clickable Start button at the left end of the top bar. Today Start opens only from the Windows key. |
| **Start button on the dock** | A button at the left end of the dock that opens the Start menu. |
| **Drag to rearrange dock icons** | Drag a pinned icon along the dock to change its place; the new order is saved to `dock_pins.txt`. Today the order can only be changed by editing that file. |
| **Custom app icons** | A section in betterUI's settings to upload your own icon for any app on the dock, replacing the one Windows provides. |
| **Themes** | Selectable colour, font and shape themes for the bar, flyouts and dock, beyond the current opacity and blur settings. |
| **betterUI settings in Windows 11 Settings** | A betterUI page reachable from the Windows Settings app. Windows has no supported way for other apps to add their own page inside Settings, so this needs research first. The likely result is a betterUI settings window designed to match Windows 11 Settings, plus an easy way to open it. |
| **Square or rounded windows toggle** | A setting to choose square or rounded corners for betterUI's own flyouts and dock. |
| **Motion polish** | Small animations that make the bar, flyouts and dock feel smooth. See [Motion polish](#motion-polish) below. |

### Motion polish

The 20 animation rules below come from a popular checklist for good UI motion. Each one is mapped to where it would apply in betterUI.
None of them are in the MVP. Today only the dock slides in and out.

| # | Rule | Where it applies in betterUI |
| --- | --- | --- |
| 1 | Ease-out for entrances | Flyouts and the dock decelerate as they appear |
| 2 | Dropdowns at 150–250 ms | Audio, Bluetooth, control center and tray flyouts open in about 200 ms |
| 3 | Stagger list rows by 30 ms | Device lists, mixer rows, Bluetooth devices and tray icons appear one after another |
| 4 | Scale to 0.97 on press | Bar buttons, dock icons, tiles and rows shrink slightly while pressed |
| 5 | Stretch moving knobs 12% | Slider knobs and on/off switch knobs stretch while moving |
| 6 | Stagger as details settle | Secondary text (percentages, "Connected", tooltips) fades in just after the main content |
| 7 | Arcs for object movement | Larger moves, such as an app being pinned on the dock, travel along a gentle curve |
| 8 | Pulse to confirm success | Brief pulse when a device is switched, an app is pinned or a radio turns on |
| 9 | Ease-in-out for moves | Dock icons sliding to make room, flyouts resizing as content changes |
| 10 | Start popovers at 0.95 | Flyouts open from 95% scale and 0 opacity to full |
| 11 | Enter from the trigger | Each flyout grows out of the bar button that opened it |
| 12 | Exit to the trigger | ...and shrinks back into it when closed |
| 13 | No bounce on everyday UI | Bar, flyouts and dock stay calm; bounce only where it means something (e.g. app launch) |
| 14 | Don't tween while held | Sliders follow the mouse exactly while dragging; easing only after release |
| 15 | Make exits 20% shorter | Closing a flyout or hiding the dock is quicker than opening it |
| 16 | Overlap old & new states | Switching the audio Devices/Mixer tabs crossfades instead of cutting |
| 17 | Masks for reveals & colour | Tiles fill with the accent colour from the click point when switched on |
| 18 | Crossfade icons + blur | The speaker icon, Wi-Fi/Bluetooth icons and mute states blend between glyphs |
| 19 | Rubber band past bounds | Lists that scroll (long mixers, many tray icons) stretch slightly at the ends |
| 20 | Reuse the same element (shared layout) | A bar button and its flyout behave as one object that expands, rather than a new window popping up |

**Ground rules**
- Animations only run during a transition, so idle CPU stays near zero.
- Everything follows the Windows setting *Settings → Accessibility → Visual effects → Animation effects*. With it off, betterUI switches instantly.
- Rules 11, 12 and 20 need the flyouts to be drawn so they can grow out of the bar. That's the biggest piece of work here and should be designed first.

## Using it

- **Multiple monitors:** every monitor gets its own bar. Flyouts open on the
  monitor you clicked. The dock stays on the main monitor.
- **Quit:** right-click the bar → *Quit betterUI*. The Windows taskbar comes back.
- **Start with Windows:** right-click the bar → *Start with Windows*. betterUI
  then starts the moment you sign in, through a task named *betterUI* in Task
  Scheduler (no admin rights needed). Windows delays apps in Task Manager's
  Startup apps list by several seconds, which let the Windows taskbar show
  first and let tray apps register with it instead of betterUI.
  From a terminal: `betterui.exe --start-with-windows=on` (or `off`).
- **Bar visibility:** right-click the bar → *Bar visibility*: always show (keeps
  its strip at the top), auto-hide (appears when the mouse touches the top edge),
  or hide when a window covers it. Also `bar_mode` in `config.ini`.
- **Look:** right-click the bar → *Bar opacity* (20–100%) and *Blur behind bar*.
  Settings are saved in `%APPDATA%\betterUI\config.ini`.
- **If the taskbar is ever stuck hidden** (for example betterUI was killed from
  Task Manager), run this in a terminal from the folder with the exe:

  ```
  betterui.exe --restore-taskbar
  ```

  This restores your original taskbar settings, including auto-hide.
- **Dock:** click an app to open or focus it (click again to minimize),
  middle-click for a new window, right-click to pin/unpin, close, or change
  when the dock hides. It starts with your taskbar pins; the list lives in
  `%APPDATA%\betterUI\dock_pins.txt`.
- **System tray:** the arrow button on the bar opens your tray icons. Click
  an icon to open its app, right-click for its menu, hover to see its
  tooltip. Can be turned off with `tray_enabled = false` in `config.ini`.
- Logs: `%APPDATA%\betterUI\log.txt`

Don't run betterUI at the same time as Seelen UI. Both replace the taskbar and
will fight over the screen edge.

## Building from source

Install Rust (https://rustup.rs) and the Visual Studio C++ Build Tools, then:

```
cargo build --release
```

## Code signing policy

*Status: builds are unsigned.* The application to the SignPath Foundation
open-source program was not approved, and another signing route is listed
under [Future work](#future-work). Until then, Windows Smart App Control may
block `betterui.exe`.

When signing is added, only binaries built by this repository's GitHub Actions
workflow from the source code in this repository will be signed, and every
signing request will be approved by hand.

Team roles:

- **Committers and reviewers:** [Thundr-cod](https://github.com/Thundr-cod)
- **Approvers:** [Thundr-cod](https://github.com/Thundr-cod)

Changes from anyone outside the list above are reviewed before they are merged.

## Privacy policy

betterUI does not collect, store or send any personal data. It makes no
network connections of its own: the network readout on the bar only reads
your PC's local connection status. Settings and logs stay on your PC in
`%APPDATA%\betterUI\`.

## License

betterUI is free software, licensed under the
[GNU General Public License v3.0](LICENSE). Copyright (C) 2026 Thundr-cod.
