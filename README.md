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
other software needed.

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
