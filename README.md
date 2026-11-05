# betterUI

A lightweight, native desktop shell for Windows 11: a top bar and a dock that
replace the Windows taskbar. Written in Rust against the Win32 API directly,
with no web view, so it starts instantly and uses a few MB of RAM.

## Status

| Part | What | State |
| --- | --- | --- |
| 1 | Top bar, taskbar replacement, RAM + network readout, clock | in progress |
| 2 | Audio flyout: devices (output / input) + per-app mixer | planned |
| 3 | Bluetooth + control center (Wi-Fi, Bluetooth, brightness, volume) | planned |
| 4 | Dock (auto-hides; also hides when a window covers it) | planned |
| 5 | System tray dropdown | planned |

## Getting it

Every push to `main` is built automatically. Download `betterui.exe` from the
**Latest build** release on the Releases page and run it. No installer and no
other software needed. See the [Code signing policy](#code-signing-policy).

## Using it

- **Quit:** right-click the bar → *Quit betterUI*. The Windows taskbar comes back.
- **Look:** right-click the bar → *Bar opacity* (20–100%) and *Blur behind bar*.
  Settings are saved in `%APPDATA%\betterUI\config.ini`.
- **If the taskbar is ever stuck hidden** (for example betterUI was killed from
  Task Manager), run this in a terminal from the folder with the exe:

  ```
  betterui.exe --restore-taskbar
  ```

  This restores your original taskbar settings, including auto-hide.
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
