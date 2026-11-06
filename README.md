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

## Future work (after the MVP)

These are planned for after `v0.1.0` and are **not** part of the MVP.

| Feature | Notes |
| --- | --- |
| **Start button on the dock** | A button at the left end of the dock that opens the Start menu. |
| **Custom app icons** | A section in betterUI's settings to upload your own icon for any app on the dock, replacing the one Windows provides. |
| **Themes** | Selectable colour, font and shape themes for the bar, flyouts and dock, beyond the current opacity and blur settings. |
| **betterUI settings in Windows 11 Settings** | A betterUI page reachable from the Windows Settings app. Windows has no supported way for other apps to add their own page inside Settings, so this needs research first. The likely result is a betterUI settings window designed to match Windows 11 Settings, plus an easy way to open it. |
| **Square or rounded windows toggle** | A setting to choose square or rounded corners for betterUI's own flyouts and dock. |

## Using it

- **Quit:** right-click the bar → *Quit betterUI*. The Windows taskbar comes back.
- **Start with Windows:** right-click the bar → *Start with Windows*. betterUI
  then starts when you sign in (it also appears in Task Manager → Startup apps).
  From a terminal: `betterui.exe --start-with-windows=on` (or `off`).
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

Windows releases of betterUI are signed through the SignPath Foundation
open-source program:
**Free code signing provided by [SignPath.io](https://about.signpath.io),
certificate by [SignPath Foundation](https://signpath.org).**

*Status: application pending. Until it is approved, builds are unsigned.*

Only binaries built by this repository's GitHub Actions workflow from the
source code in this repository are signed. Every signing request is approved
by hand.

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
