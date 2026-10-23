# toolswitch

Small everyday tools in one tiny app that lives in the tray. Linux, Windows and macOS.
Everything runs on your computer: nothing is uploaded, no account.

About 90 MB of memory and almost no processor while it waits in the tray. Tools that work by
themselves share one background thread that wakes up every few seconds; a tool that is off
does nothing at all.

## Tools

**Everyday**

| Tool | What it does | Shortcut (Linux · Windows · macOS) |
|---|---|---|
| Clipboard history | Keeps what you copy; search it and paste an old text. Pin the ones you use often. | Super+Shift+V · Ctrl+Alt+V · Cmd+Option+V |
| Text from screen | Select part of the screen and copy the text in it (OCR, letters without accents). | Super+Shift+X · Ctrl+Alt+X · Cmd+Option+X |
| Wallpapers | Pick your wallpapers, switch them with a shortcut or every few minutes. | Super+Shift+W, Super+W (random), Super+. / Super+, |
| Reminders | Write "in 20 min call Juan" or "at 15:30 meeting" (English or Spanish). | Super+Shift+R · Ctrl+Alt+R · Cmd+Option+R |
| Breaks | Reminds you to rest your eyes (20-20-20) or works as a Pomodoro timer. | |

**Files**

| Tool | What it does |
|---|---|
| Compress | Smaller JPG, PNG, PDF and Office files, saved next to the original as name-min. |
| Remove background | Transparent PNG of any photo, with an AI model (downloaded once, 170 MB). |
| Sign PDF | Draw your signature once; drop PDFs and get name-signed.pdf with it. |
| Tidy downloads | Moves downloads to Documents, Pictures, Videos... by type. Every move can be undone. Off by default. |
| Backup | Copies the folders you pick to another disk every day. Only what changed; nothing is deleted there. Off by default. |

On Linux, Compress, Remove background and Sign PDF are also in the Files right-click menu
(Scripts).

**System**

| Tool | What it does |
|---|---|
| Clean up | Shows the trash, app caches, old downloads and big files, and frees them (to the trash when possible). |
| Watch the PC | Warns when a disk is almost full or a program keeps the processor or memory busy; close it with a click. |
| Shut down timer | Shut down, restart or sleep in a while, at a time, or when the downloads finish. |
| Updates | Updates the system and apps with one click: apt, dnf, pacman, Flatpak, Snap, winget, Homebrew, Software Update. Checks once a day. |

Every shortcut can be changed in the Tools screen: click the keys and press new ones.

## Command line

```
toolswitch                    open the window
toolswitch --hidden           start in the tray only (used at login)
toolswitch clipboard | ocr | reminders | wallpaper
toolswitch wallpaper-next | wallpaper-prev | wallpaper-random
toolswitch compress | cutout | sign  [files or folders]
```

A second launch hands its command to the running app and exits.

## Install

Download the file for your system from the [releases](../../releases):

| System | File | How |
|---|---|---|
| Ubuntu, Debian, Mint | `toolswitch_X_amd64.deb` | Double-click it, or `sudo apt install ./toolswitch_X_amd64.deb` |
| Other Linux | `toolswitch-vX-linux-x86_64.tar.gz` | Unpack it and run `toolswitch` |
| Windows 10/11 | `toolswitch-X-windows-x86_64-setup.exe` | Run it. No administrator needed. Windows may say "unrecognized app": More info > Run anyway |
| Mac with Apple chip (M1 and later) | `toolswitch-vX-macos-arm64.dmg` | Drag toolswitch to Applications |
| Mac with Intel chip | `toolswitch-vX-macos-x86_64.dmg` | Same |

The installers aren't signed with a paid certificate yet, hence the warnings on Windows and
macOS.

## Where things are

| | Linux | Windows | macOS |
|---|---|---|---|
| Settings | `~/.config/toolswitch` | `%APPDATA%\toolswitch` | `~/Library/Application Support/toolswitch` |
| Cache (thumbnails, AI models) | `~/.cache/toolswitch` | `%LOCALAPPDATA%\toolswitch` | `~/Library/Caches/toolswitch` |

## Notes per system

- **Linux:** made for GNOME on X11 (shortcuts are GNOME custom keybindings; the tray needs the
  AppIndicator extension, which Ubuntu has). On Wayland, Text from screen can't take the
  screenshot yet.
- **Windows:** Windows already uses many Windows-key shortcuts, so the defaults are Ctrl+Alt.
- **macOS:** the app isn't signed yet: the first time, right-click it and choose Open (on
  macOS 15, then System Settings > Privacy & Security > Open Anyway). Text from screen needs
  the Screen Recording permission.

## Building

```
cargo build --release
```

Pure Rust, no GTK or other system libraries needed. Pushing a `v*` tag builds the installers
and attaches them to the GitHub release (`.github/workflows/release.yml`): the `.deb` with
`packaging/linux/build-deb.sh`, the Windows setup with Inno Setup (`packaging/windows`) and the
macOS `.dmg` with `hdiutil`. The icon is drawn in `assets/icon.svg`.

Tests: `cargo test --release`. Some take a file and are ignored by default:

```
OCR_TEST_IMAGE=shot.png        cargo test --release ocr_image -- --ignored --nocapture
CUTOUT_TEST_IMAGE=photo.jpg    cargo test --release cutout_image -- --ignored --nocapture
SIGN_TEST_PDF=file.pdf         cargo test --release sign_a_pdf -- --ignored --nocapture
COMPRESS_TEST_DIR=some/folder  cargo test --release compress_folder -- --ignored --nocapture
```

## License

MIT. The background remover uses ISNet (Apache 2.0); text from screen uses ocrs (MIT/Apache 2.0).
