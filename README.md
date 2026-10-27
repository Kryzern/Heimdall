<p align="center">
  <img src="docs/logo.svg" width="120" alt="Heimdall logo">
</p>

<h1 align="center">Heimdall</h1>

<p align="center">
  <b>A fast, tiny, glass launcher for Windows.</b><br>
  Press <kbd>Alt</kbd> + <kbd>Space</kbd>, type a few letters, hit <kbd>Enter</kbd>. That's it :)
</p>

<p align="center">
  <a href="https://github.com/Kryzern/Heimdall/releases/latest"><img alt="Latest release" src="https://img.shields.io/github/v/release/Kryzern/Heimdall?style=flat-square&color=8b93a7"></a>
  <a href="https://github.com/Kryzern/Heimdall/releases"><img alt="Downloads" src="https://img.shields.io/github/downloads/Kryzern/Heimdall/total?style=flat-square&color=8b93a7"></a>
  <img alt="Windows 10 and 11" src="https://img.shields.io/badge/windows-10%20%7C%2011-8b93a7?style=flat-square">
  <img alt="Under 2 MB" src="https://img.shields.io/badge/size-under%202%20MB-8b93a7?style=flat-square">
  <a href="LICENSE"><img alt="MIT license" src="https://img.shields.io/github/license/Kryzern/Heimdall?style=flat-square&color=8b93a7"></a>
</p>

<p align="center">
  <a href="#install">Install</a> ·
  <a href="#using-it">Using it</a> ·
  <a href="#settings">Settings</a> ·
  <a href="#benchmarks">Benchmarks</a> ·
  <a href="#building-from-source">Building</a>
</p>

<p align="center">
  <img src="docs/glass.png" width="720" alt="Heimdall open over a colourful window">
</p>

## Why I built it

I wanted something like Spotlight on Windows that opens instantly, finds any file on any drive, and actually looks good doing it. I'd been using Flow Launcher for around a year and a half before this, and it's great, but it's a 250 MB .NET app with a plugin system I never really used. Everything is incredibly fast, but it's a file list, not a launcher.

Heimdall sits somewhere in between. It's two native executables, under 2 MB together, with no Electron, WebView, .NET or plugin runtime. It searches your apps, files and Windows settings, does quick maths, switches windows, and stays out of your way the rest of the time. Since switching, I honestly haven't felt the need to go back :)

## What people are saying

> Yes, this thing is fast af.
>
> <sub>r/windowsapps</sub>

> Its flawless app, i really love it.
>
> <sub>[issue #1](https://github.com/Kryzern/Heimdall/issues/1)</sub>

> It's lightweight, looks beautiful, and the search speed is excellent. I can see myself using it as my daily launcher.
>
> <sub>[issue #2](https://github.com/Kryzern/Heimdall/issues/2), switching from Flow Launcher</sub>

Thank you all for the lovely feedback, it genuinely made my day. Most of 1.0.1 came straight from your comments and issues.

## What's new in 1.0.3

- **Leave folders and drives out of search.** Add them to `exclude` in the config, like `exclude = ['D:', 'C:\Windows', 'node_modules']`, or type `-in:node_modules` for a single search.
- **Corners**: round, Windows (the same smaller radius Windows 11 uses) or square.
- **Animations can be turned off**, so everything appears instantly.

## What's new in 1.0.2

- **The hotkey no longer stops working** after the first press when a maximized window is open on a screen without a visible taskbar (auto-hide, or a second monitor). Game mode thought those windows were fullscreen games.
- **Upgrading keeps the file indexer running.** Before, installing over an older version could leave the indexer service stopped.
- **Base64.** Type `b64 hello` to encode or `b64d aGVsbG8=` to decode, Enter copies the result.
- **Scoop support**, see [Install](#install).

## What's new in 1.0.1

- **Alt+Space by default**, and you can change the hotkey from inside Heimdall now. Just type `settings`.
- **A settings page** for the hotkey, theme, glass, shadow, tray icon and starting with Windows. No more digging through a config file for the basics.
- **A tray icon**, so there's always an obvious way to get to settings or quit.
- **Windows settings search.** Type `bluetooth`, `dark mode`, `display` or `uninstall` and jump straight to the right page.
- **A right-click menu** on results: Open with, Open file location, Copy path, Properties and Run as administrator.
- **More filters**, like `type:image`, `type:video` or `type:code`, plus a `?` page that lists all of them.
- **An adjustable shadow** (normal, soft or off), and a cleaner light mode.
- **A proper installer**, which also sets up the fast file indexer for you.
- **Single letter searches are over 2x faster**, and two letter ones got quicker too, so Heimdall now beats Everything on every query in the benchmark below.

## Install

1. Download `Heimdall-1.0.3-x64-setup.exe` from the [latest release](https://github.com/Kryzern/Heimdall/releases/latest) and run it. Windows will ask for admin once, which is for the file indexer service.
2. Press <kbd>Alt</kbd> + <kbd>Space</kbd>.

The installer puts Heimdall in `%LOCALAPPDATA%\Programs\Heimdall`, adds it to the Start menu and the tray, starts it, and sets it to start with Windows.

<details>
<summary><b>The indexer service</b></summary>
<br>

The installer also sets up `HeimdallIndexer`, a small Windows service that makes file search a lot faster and lighter. It reads the NTFS master file table directly, keeps up with changes through the USN journal, and holds the index outside the launcher process. It lives in `C:\Program Files\Heimdall`.

If you say no to the admin prompt, that's completely fine, Heimdall still works and indexes your drives by itself. You can always add the service later by typing `install indexer` into Heimdall.

</details>

<details>
<summary><b>Without the installer</b></summary>
<br>

The release also has `Heimdall-1.0.3-x64.zip`. Unzip it, right-click `install.ps1` and choose **Run with PowerShell**. It installs the indexer service too, but you can run it from a terminal with `-NoIndexer` to skip that. `uninstall.ps1` removes everything again.

</details>

<details>
<summary><b>With Scoop</b></summary>
<br>

```powershell
scoop bucket add heimdall https://github.com/Kryzern/Heimdall
scoop install heimdall/heimdall
```

This installs the portable version, so `scoop update` keeps it up to date. Turn on **Start with Windows** in Heimdall's settings, and type `install indexer` if you want the faster file indexer too.

</details>

<details>
<summary><b>Uninstalling</b></summary>
<br>

Uninstall Heimdall from **Settings > Apps**. That stops it, removes the autostart entry and the indexer service, and deletes the program folder. Your settings stay in `%APPDATA%\Heimdall` until you delete that folder yourself.

</details>

## Using it

Type and press Enter. Apps, files, folders and everything below come back in one ranked list, and the results you pick move up for the same query next time. If you ever forget something, just type `?`.

| Type | What happens |
|---|---|
| `chr` | Apps by prefix, word or acronym (`vsc` finds Visual Studio Code) |
| `report ext:pdf` | Files, with filters: `ext:`, `in:`, `type:folder`, `size:>100mb`, `mod:week` |
| `notes -in:node_modules` | Leave a folder, path or drive out of the results (`-in:D:`, `-in:"C:\Program Files"`) |
| `holiday type:image` | File types: `image`, `video`, `audio`, `doc`, `code`, `archive`, `app` |
| `bluetooth`, `dark mode` | Windows settings pages, and tools like Device Manager |
| `heimdall\src main` | Path fragments: `main` somewhere under a `heimdall\src` folder |
| `2^10 * 3` | Calculator, Enter copies the answer |
| `b64 hello`, `b64d aGVsbG8=` | Base64 encode and decode, Enter copies the result |
| `>` | Open windows, pick one to switch to it |
| `C:\Us` or `~\Doc` | Browse folders, Tab completes |
| `g rust lifetimes` | Web search with a prefix (`g`, `ddg`, `yt`, `gh`, `wiki`, or your own) |
| `lock`, `sleep`, `restart` | System actions, anything destructive asks for a second Enter |
| `settings` | Heimdall's own settings |
| `?` | Tips, with every filter and prefix one Enter away |

| Key | Action |
|---|---|
| <kbd>Alt</kbd> + <kbd>Space</kbd> | Show or hide |
| <kbd>Enter</kbd> | Open |
| <kbd>Ctrl</kbd> + <kbd>Enter</kbd> | Show in Explorer |
| <kbd>Ctrl</kbd> + <kbd>Shift</kbd> + <kbd>Enter</kbd> | Run as administrator |
| <kbd>Alt</kbd> + <kbd>Enter</kbd> | Properties |
| <kbd>Shift</kbd> + <kbd>Space</kbd> | Preview: thumbnail, text, or folder contents |
| <kbd>Tab</kbd> | Put the selected folder's path in the search box |
| <kbd>Ctrl</kbd> + <kbd>C</kbd> | Copy the selected path |
| Right-click, <kbd>Shift</kbd> + <kbd>F10</kbd> | Open with, Open file location, Copy path, Properties, Run as administrator |
| <kbd>Ctrl</kbd> + <kbd>Shift</kbd> + <kbd>Delete</kbd> | Forget the selected item from history |
| <kbd>Esc</kbd> | Close preview, then clear, then hide |

You can also drag any file result straight into another app.

## Settings

Type `settings` in Heimdall, or right-click the tray icon and pick **Settings**. To change the hotkey, select **Hotkey**, press Enter, then press your new shortcut. Theme, glass, shadow, corners, animations, the tray icon and starting with Windows all change with one Enter each.

Everything else lives in `%APPDATA%\Heimdall\config.toml`, which gets created the first time Heimdall runs. Changes there apply the next time Heimdall opens.

```toml
hotkey = "Alt+Space"          # or "F5", "Ctrl+Shift+K", ...
passthrough = ["chrome.exe"]  # apps where the hotkey keeps its normal meaning
game_mode = true              # release the hotkey while a fullscreen game has focus
theme = "system"              # "dark" or "light"
accent = "system"             # or a colour like "#7C9CFF"
glass = "liquid"              # "auto" follows Windows transparency, "solid" is opaque
shadow = "normal"             # "soft" or "off"
corners = "round"             # "windows" for Windows 11's smaller corners, or "square"
animations = true             # false makes everything appear instantly
exclude = ['D:', 'C:\Windows', 'node_modules']  # left out of file search
width = 680.0
max_results = 8
show_recent = true
start_with_windows = true
tray = true                   # icon in the notification area

[[web]]
key = "g"
name = "Google"
url = "https://www.google.com/search?q={q}"
```

If you pick a key that apps use themselves, like F5, add those apps to `passthrough` so the key still works there. Coming from Flow Launcher? Type `import flow` to bring your web searches over.

## The glass

Most glass effects are just a blur with a white border. I wanted the panel to feel like an actual piece of glass, which mostly means paying attention to the edges.

- Near the rim, the background is pulled from just outside the panel, so whatever is behind it bends around the border.
- Red and blue split very slightly in that bevel.
- A thin highlight sits on the side of the rim facing a light, and the light follows the selected row.
- The frost adapts to whatever's behind each pixel, so text stays readable over a white page or a dark wallpaper.
- The selection is a smaller piece of thicker glass with its own rim, sliding between rows on a spring.

All of it is one pixel shader. The desktop behind the panel is captured once when Heimdall opens, mip-mapped on the GPU, and sampled from there. Nothing is drawn unless something changes, so Heimdall uses no CPU or GPU while it's hidden. It drops to a solid panel on battery saver or over Remote Desktop.

## How it's fast

- **One arena per drive.** Every filename on a volume sits in one contiguous block of bytes, with a lowercase copy at the same offsets. A search is a SIMD substring scan over that block, split across every core.
- **Three stages.** The scan finds candidates, a cheap name score narrows them down, and only the best few hundred get their full path built and scored. Folders like `node_modules`, caches and `AppData` rank lower, while your Desktop, Documents and Downloads rank higher.
- **Short queries take a different path.** One or two letters match most of the index, so Heimdall skips the candidate list completely and scores every name in a single pass, keeping only the best few hundred as it goes.
- **Typing narrows.** When `repo` becomes `repor`, Heimdall filters the previous results instead of scanning again.
- **Input comes first.** The window handles every pending key before drawing a frame, and file search runs at a lower priority than the UI.
- **No paths in memory.** Each file is a 16-byte entry. Full paths are only built for the rows actually on screen.

## Benchmarks

`heimdall-bench` (included in the zip) builds the index and times queries against Heimdall's engine and against a running copy of [Everything](https://www.voidtools.com/). These numbers are from my machine: 8 threads, four drives, 3.77 million files, median of 20 runs.

| Query | Matches | Heimdall | Everything |
|---|---:|---:|---:|
| `a` | 2.47 M | **11.3 ms** | 23.1 ms |
| `re` | 497 k | **11.2 ms** | 14.8 ms |
| `report` | 3,081 | **4.1 ms** | 13.8 ms |
| `main.rs` | 79 | **3.2 ms** | 13.6 ms |
| `readme` | 12,726 | **4.6 ms** | 14.0 ms |
| `node_modules` | 402 | **3.5 ms** | 12.6 ms |
| `ext:pdf invoice` | 1 | **3.2 ms** | 8.0 ms |

| | Heimdall | Everything 1.5 | Flow Launcher 2.1.3 |
|---|---:|---:|---:|
| On disk | 1.9 MB | 6.1 MB | 247 MB |
| Index memory, 3.77 M files | 289 MB | 342 MB | n/a |

To be fair to Everything, these aren't perfectly like for like:

- Heimdall's times are measured inside the engine, while Everything's include its IPC round trip.
- Everything returns matches sorted by name. Heimdall returns them ranked, which in my opinion is the part that actually saves you time.
- The index here was built by the directory-walk fallback. Run the benchmark from an elevated prompt to measure the MFT path.

## Building from source

You'll need Rust on Windows.

```powershell
git clone https://github.com/Kryzern/Heimdall.git
cd Heimdall
cargo test --workspace
powershell -ExecutionPolicy Bypass -File .\packaging\package.ps1   # writes the zip, and the installer if Inno Setup 6 is installed
```

The workspace has four crates:

| Crate | What it is |
|---|---|
| `heimdall` | The launcher: window, input, ranking, Direct3D glass, Direct2D text |
| `heimdall-core` | The index, query language, scoring and the pipe protocol |
| `heimdall-indexer` | The Windows service that reads the MFT and tails the USN journal |
| `heimdall-bench` | The benchmark above |

Issues and pull requests are always welcome, I'm happy to take a look at anything :)

## Privacy

Nothing leaves your machine. There's no telemetry and no update check, and Heimdall makes no network requests apart from the web searches you open yourself. Usage history is a local JSON file. Like Everything, the indexer service sees filenames across the whole drive, so any local user can search names (not contents).

## License

MIT
