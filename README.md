<p align="center">
  <img src="docs/icon.png" alt="Kanso" width="128">
</p>

[English](README.md) | [简体中文](README.zh-CN.md)

**A small terminal for local shells and SSH — and nothing in the way.**

Kanso does two things: it opens a shell on this machine, and it opens a shell on
another one. Everything in it serves those two jobs, and the rest of the window
stays out of your way while you work.

Built with **Rust + Tauri**: one native binary, installers of 4–5 MB, no Electron
runtime, no bundled fonts, no daemon, no telemetry.

<img src="docs/screenshot-dark.png" alt="Kanso, dark theme" width="100%">

<img src="docs/screenshot-light.png" alt="Kanso, light theme" width="100%">

## Small, and it stays small

| Package | Download |
| --- | --- |
| Windows x64 installer (`.exe`) | **3.9 MB** |
| macOS Apple Silicon (`.dmg`) | **4.7 MB** |
| Linux `.deb` (x64 / ARM64) | **5.3 MB** / **5.2 MB** |

The interface is a web view; the terminal, the SSH client, the PTY and the file
transfer are Rust. Starting Kanso is a window appearing, not a runtime booting.

## Local shell

A real pseudoterminal, resized with the window. The Shell field takes a command
line with arguments, so `wsl.exe -d Ubuntu` or `pwsh -NoLogo` is a session. On
macOS a bare shell starts as a login shell, the way Terminal.app starts it, so
`~/.zprofile` and Homebrew's `PATH` are in place before `~/.zshrc` runs.

A thin rule is drawn above each prompt, so one command's output is easy to tell
from the next.

## SSH

Password, public-key and ssh-agent authentication, followed by the
keyboard-interactive rounds a server uses to ask for a second factor — a
one-time code, a push confirmation. Jump hosts (ProxyJump) connect through
another saved session, chained when a route needs more than one hop.

**Old devices connect too.** Switches, routers and firewalls often run SSH
servers that stop at algorithms modern clients no longer offer. Kanso offers the
SHA-1 key exchanges, the CBC ciphers and the SHA-1 MACs as well — placed after
every modern algorithm, with no setting to change. A server that supports
anything better gets that, and because both sides' algorithm lists are signed by
the server's host key, nobody in between can strip the better choices to force
the old ones. A session that needed one says **Legacy SSH** in the status bar.

**Encodings.** UTF-8 unless the session says otherwise: a device speaking
GB18030 / GBK, Big5, Shift_JIS, EUC-JP, EUC-KR or a Windows / KOI8 code page has
its output decoded for the terminal and your typed input encoded for the far end.

**Files, without a second login.** The Filer panel rides the same SSH
connection, so browsing and transferring files costs no second authentication:
drag files in either direction, or jump straight to the directory the shell is
in.

## The terminal

- xterm.js rendering through WebGL, with the Unicode 11 width tables.
- A timestamp and line-number gutter, so a long session stays readable.
- Search, a right-click menu, middle-click paste, and copy-on-select if you want
  it.
- Split panes that nest, each with its own tab strip.
- No fonts bundled: the font fields list what is installed on this machine and
  accept any name you type, so a private Nerd Font build works, and Nerd Font
  glyphs fall back to it automatically.
- Dark and light themes, and every shortcut rebindable.

## Install

| Platform | Package |
| --- | --- |
| Windows x64 | NSIS installer (`.exe`) and a portable `.zip` |
| macOS Apple Silicon | `.dmg` |
| macOS Intel | `.dmg` |
| Linux x64 / ARM64 | `.AppImage` and `.deb` |

Downloads are on the [Releases page](https://github.com/chhuax/Kanso/releases/latest).

The Windows portable zip keeps every setting in a `data` folder beside the
executable, so the whole folder can move between machines or live on a removable
drive; the AppImage runs in place on Linux without installation. Releases are
not notarized on macOS or code-signed with Windows Authenticode, so the system
may show a security warning on first install.

Automatic updates are off in this build. **Help → Check for Updates…** opens the
Releases page in your browser, where you download the new version yourself.

## Origin

Kanso is a **modified** fork of
[EdgeTerm](https://github.com/miskin-lee/EdgeTerm) by miskin-lee, forked at
commit `39d35ca` (v0.8.3) on 17 September 2026 and changed since. It is an
independent project: not affiliated with, sponsored by, or endorsed by EdgeTerm
or its author, and not EdgeTerm itself. Copyright in the upstream portions of the
code remains with the EdgeTerm author; copyright in the changes is held by the
Kanso contributors. `git log 39d35ca..HEAD` is the record of what changed, and
[NOTICE](NOTICE) carries the modification notice that GPL-3.0 section 5(a) asks
for.

## License

Kanso is licensed under the [GNU General Public License v3.0](LICENSE) **only**
(`GPL-3.0-only`), inherited from EdgeTerm. It is free software: you may
redistribute and modify it under the same terms, and any derivative work you
distribute must be released under the GPL-3.0 with its complete corresponding
source code. It comes with **no warranty**.

Bundled third-party components:

- **Material Icon Theme** — the file-type icons in the Filer panel. MIT,
  Copyright (c) 2025 Material Extensions.
- **Codicons** — the interface icons, by
  [Microsoft](https://github.com/microsoft/vscode-codicons). Used under CC BY 4.0.

Their licence texts and attribution statements are in [NOTICE](NOTICE).
