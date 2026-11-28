# GodRemote

**English** · [Español](README.md)

A remote connection manager written in Rust, in the style of MobaXterm: a
hierarchical tree of hosts, tabbed terminals (SSH, Telnet, serial port), an
SFTP browser, built-in remote desktops (RDP and VNC), its own X server for
remote graphical applications, tunnels with their own manager, a key
manager, SSH authentication with certificates, a second factor (TOTP) and
security keys, snippets with shortcuts, split view and simultaneous typing
in several terminals, triggers, session recording and command history, a
status dashboard for the hosts in a folder, settings inherited per folder,
import from other tools, encrypted sync of the configuration between your
computers, folders of hosts shared with teammates, customizable themes, an
interface in Spanish and English, and an encrypted vault for credentials.

**Downloads:** the
[latest published release](https://github.com/marc-mora/GodRemote/releases/latest)
has, for each of the two editions, an installer and a portable `.zip` for
Windows, a universal application (`.dmg`, Intel and Apple Silicon) for macOS
and a `.deb` package and a `.tar.gz` for Linux, with their checksums in
`SHA256SUMS.txt`. The executables are not signed: Windows and macOS warn you
the first time.

![SSH terminal](docs/terminal.png)

*The screenshots show the interface in Spanish.*

![Server info bar and terminal background](docs/barra-y-fondo.png)
![Split view with broadcast](docs/vista-dividida.png)
![Typing in several terminals at once](docs/difusion.png)
![Tabs with custom names, activity notices, links and highlighted words](docs/pestanas-y-enlaces.png)
![Tunnel manager](docs/tuneles.png)
![Command palette](docs/paleta.png)
![Confirmation when pasting several lines, and folder color](docs/protecciones.png)
![One command on several hosts](docs/varios-equipos.png)
![Status dashboard for a folder, with alerts](docs/panel-estado.png)
![Vault viewer](docs/baul.png)
![Syncing folders over SFTP](docs/sftp-sincronizar.png)
![Built-in X server](docs/servidor-x.png)
![Snippets with keyboard shortcuts](docs/snippets.png)
![Built-in remote desktop](docs/escritorio.png)
![Themes](docs/temas.png)
![Theme following the system](docs/tema-automatico.png)
![Settings](docs/ajustes.png)

## What it does

| Area | Feature |
|------|---------|
| **Hosts** | Tree of folders and subfolders, search, move, duplicate, rename and delete. Quick connect `user@host[:port]`. |
| **Folders with values** | A folder can give its hosts the user, the authentication, the jump hosts, the terminal background and the info bar. |
| **Import and backup** | Imports hosts from `~/.ssh/config`, PuTTY and MobaXterm. Backup of the whole configuration in a single file. |
| **Sync** | The configuration (hosts, snippets, tunnels, themes, settings and the vault) is kept the same on all your computers through a folder you already replicate (Dropbox, Syncthing, a network drive…), in a single encrypted file. Item-by-item merge; clashing changes are asked about. |
| **Shared folders** | Publish a folder of hosts in an encrypted file for your teammates (without passwords or commands) or subscribe to someone else's: it appears in the tree, read-only, and updates itself. |
| **SSH** | Built-in terminal (xterm-256color, 10,000-line scrollback, selection and copy/paste, mouse for `htop`, `mc`, `vim`…). |
| **SSH authentication** | Password, keyboard-interactive (PAM, OTP codes), private key (OpenSSH, PEM, PuTTY `.ppk`), OpenSSH certificates, SSH agent (the OpenSSH one and, on Windows, Pageant) and FIDO2 security keys through the agent. The second-factor code (TOTP) can be filled in automatically when connecting. |
| **Jump hosts (bastions)** | Each host has its own ordered list of SSH jump hosts, like `ProxyJump a,b,c`. Works for SSH, SFTP, Telnet, VNC and built-in RDP. |
| **Tunnels** | Local (`-L`), remote (`-R`) and dynamic SOCKS5 (`-D`) forwarding. Tied to a session (they open and close with its terminal) or saved in the **tunnel manager**, which starts and stops them without opening a terminal. |
| **X11** | Its own X server, written in Rust and built in: remote graphical applications (classic, GTK 3, GTK 4, Qt 5, Tk, Motif, Java) are shown in a tab without installing VcXsrv, Xming or XQuartz, with their windows (move, resize, minimize, maximize) and the pointer each application asks for; experimentally, each one in its own system window. The system X server can also be used. Forwarding over SSH with an authentication cookie. |
| **Agent and proxy** | Forwarding the SSH agent to a host (like `ssh -A`) and going out through a corporate HTTP or SOCKS5 proxy. |
| **Host verification** | Uses `~/.ssh/known_hosts`, shared with OpenSSH. Asks the first time and **rejects** the connection if the server's key changes. |
| **Terminal** | Search on screen and in the scrollback, logging the session to a file, automatic reconnection if the network drops and reopening the tabs on startup. Links that open with `Ctrl+click` and highlighted words (errors in red, warnings in yellow…) with custom rules. |
| **Triggers, recording and history** | Rules that notify you (or, if explicitly asked to, answer) when some text appears in a terminal's output, and a notice when a long command finishes in a tab you are not looking at. Session recording with its timing (asciicast, `.cast`) and a built-in player. History of the commands run on each host, with search. |
| **Tabs** | They are reordered by dragging, renamed with a double-click, carry the color of their folder (production in red) and let you know when there is new output (or the bell rings) in one you are not looking at. |
| **Command palette** | `Ctrl+Shift+P` and type: jump to a host, a tab, a snippet, a tunnel or an action. Shortcuts to switch, open and close tabs. |
| **Safeguards** | Asks before pasting several lines and before closing tabs (or the application) with connected sessions. |
| **Automation** | Commands that are sent automatically when connecting to a host, and running a command on all the hosts in a folder, collecting the output and the exit code of each one. |
| **Split view** | Several terminals at once, tiled; can be combined with broadcast. |
| **SFTP** | Two panes (local / remote), recursive upload and download with progress and cancellation, create folder, rename and delete. Drag and drop from the system, permissions (`chmod`), resuming half-finished transfers and syncing folders with a plan shown beforehand. It can be opened over a terminal's SSH connection, follow its folder and edit remote files with the local editor. Dates in local time. |
| **RDP** | Built-in client in a tab, or the system's external client (FreeRDP / `mstsc`). |
| **VNC** | Built-in client (RFB 3.3–3.8; Raw, CopyRect, Hextile, ZRLE; VNC password). |
| **Telnet and serial** | Terminals for Telnet and for serial ports (`COM3`, `/dev/ttyUSB0`…). |
| **Snippets** | Named texts and commands that are sent to the terminal from the menu or with their own keyboard shortcut (`Ctrl+Alt+D`, `F6`…). They are written by hand or recorded by typing, and accept variables (`{{host}}`, `{{?Question}}`). |
| **Broadcast** | What you type in one terminal is sent at the same time to all the open terminals or only to the ones you choose. |
| **Info bar** | Under each SSH terminal: server name, system, uptime, CPU, memory, disk, network… You choose what is shown, in what order and how often it is updated. |
| **Status dashboard** | All the SSH hosts in a folder at once: connection, latency, CPU, load, memory, fullest disk, uptime and a trend sparkline; threshold alerts with history. |
| **Themes** | Light or dark according to the operating system, changing along with it. Five built-in themes and a live editor for colors, fonts and font sizes, line spacing, spacing and borders. Import and export as JSON. |
| **Terminal background** | A text, an ASCII drawing or an image behind the text, with whatever opacity you want. Per theme or per host. |
| **Keys** | Lists the keys in `~/.ssh` and its own, with their certificates, and what is loaded in the agent; generates Ed25519 / ECDSA pairs (with an optional passphrase), imports keys and copies the public key. |
| **Credentials** | Encrypted vault with a master password (Argon2id + XChaCha20-Poly1305). Passwords are only saved after a successful authentication. It locks itself after a while without activity and has a viewer to review and delete what is stored. |
| **Language** | Spanish or English: the operating system's, or the one you choose in the settings. |
| **Local terminal** | With the shell you choose: PowerShell, cmd, WSL or Git Bash on Windows; bash, zsh, fish… on Linux and macOS. |

## Building and running

You need Rust 1.88 or later, a C compiler, CMake and NASM (the cryptographic
libraries use them).

```bash
cargo run --release
```

On Linux you also need the usual desktop libraries. On Debian/Ubuntu:

```bash
sudo apt install build-essential cmake libxkbcommon-x11-0 libgl1 libegl1
```

### Automated builds and tests

Every change that is pushed goes through GitHub Actions
(`.github/workflows/ci.yml`):

- **Linux**: formatting, all the tests of both editions (including the ones
  that need real servers: `sshd`, `telnetd`, `x11vnc`, FreeRDP, X clients)
  and `clippy` with no warnings.
- **Windows and macOS**: native build and the tests that do not depend on
  Unix servers.
- **On all three**, the real application: it opens, paints its window, saves
  a screenshot (kept as an artifact of the run) and closes. This is what
  `GODREMOTE_CAPTURA=fichero.png godremote` does, and the exit code says
  whether the window actually got painted.

Creating a `vX.Y.Z` tag also builds the native executables of both editions
for Windows, Linux and macOS, and publishes them as a **Release** on GitHub.
The tag can be pushed with `git` or created from the web (*Releases → Draft
a new release*, typing the new tag and publishing): in that case the
executables are added to the newly created release. If
`docs/versiones/vX.Y.Z.md` exists, it serves as the release notes. The same
executables can be requested without publishing a release (they are
downloaded as artifacts of the run, in the *Actions* tab): by hand from that
tab, or by putting `[ejecutables]` in a commit message.

The packages are prepared by `empaquetado/empaquetar.sh` from the already
built executables (it works just the same by hand): `.tar.gz` and `.deb` on
Linux (built on an old Ubuntu so that it works on more distributions),
`.tar.gz` and `.dmg` with the universal application on macOS, and `.zip` and
a per-user installer (Inno Setup, no administrator rights) on Windows.
Before packaging, each executable goes through the startup test.

### What the window is rendered with

On Linux and macOS, OpenGL. On **Windows, DirectX 12**: it also works where
there is no OpenGL driver (virtual machines, servers without a graphics
card, some remote desktop sessions), because DirectX comes with a software
mode. If DirectX does not start, OpenGL is tried, and if neither works a
dialog box says why. The `GODREMOTE_RENDER` environment variable (`directx`
or `opengl`) picks one by hand.

### Windows executable from Linux

With `mingw-w64` and `nasm` installed:

```bash
rustup target add x86_64-pc-windows-gnu
```

```bash
cargo build --release --target x86_64-pc-windows-gnu
```

The result is a single `godremote.exe` that depends only on system DLLs.

### Two editions: full and "hardened SSH"

The built-in RDP client depends on IronRDP, whose dependencies pin a
pre-release version of `rsa` that prevents Cargo from choosing the latest
version of the SSH library (russh). That is decided at build time, so there
are two editions of the same code:

| Edition | Built-in RDP | SSH library | How it is built |
|---------|--------------|-------------|-----------------|
| **Full** (`godremote`) | Yes | russh 0.65.1 | `cargo build --release` |
| **Hardened SSH** (`godremote-reforzado`) | No (RDP with the external client) | russh 0.65.4 | `cargo build --release --manifest-path reforzado/Cargo.toml --target-dir target` |

Both can be downloaded prebuilt, for Windows, Linux and macOS, from the
[releases page](https://github.com/marc-mora/GodRemote/releases)
(`GodRemote-…` is the full one and `GodRemote-ssh-reforzado-…` the hardened
SSH one). They use the same configuration folder, so you can switch from one
to the other.

Version 0.65.1 lacks several hardening measures against malicious SSH
servers, released between 0.65.2 and 0.65.4. If you do not use the built-in
RDP, or you connect to servers you do not fully trust, use the hardened
edition. **⚙ Settings** says which edition and which version of the library
you are using; in the full edition it also lets you turn off the built-in
RDP so that RDP always opens with the external client (that changes the
client, not the library).

`reforzado/` has its own `Cargo.toml` and `Cargo.lock` because a
`Cargo.lock` also includes the optional dependencies; a test checks that the
two dependency lists do not drift apart. When IronRDP stops pinning that
version, `cargo update -p russh` will be enough and the second edition will
no longer be needed.

## Usage

- **➕ Session** creates a host; **📁 Folder** creates a folder inside the
  selected one.
- **Double-clicking** a host connects. **Right-clicking** offers edit,
  duplicate, move, delete and the alternative ways of opening it.
- In the terminal: `Ctrl+C` copies if there is selected text and otherwise
  sends the interrupt; `Ctrl+V` pastes; `Shift+PgUp/PgDn` or the wheel
  scroll through the scrollback. When a remote application uses the mouse,
  `Shift` gives the mouse back to local selection.
- On the tabs: **drag** one to move it, **double-click** to rename it (Enter
  saves, Escape cancels), middle-click to close it and **right-click** to
  duplicate it, move it to the beginning or the end, and close the others or
  the ones to its right. The name you give it is kept when the tabs are
  reopened.
- In SFTP: a double-click enters a folder or transfers a file; `Ctrl+click`
  selects several.
- In a remote desktop: the keyboard and the mouse go to the remote host
  while the tab has the focus. `Ctrl+V` sends the local clipboard first. The
  **Ctrl+Alt+Del** and **Win** buttons send those keys, which the local
  system does not allow to be captured.

### Command palette and shortcuts

**Ctrl+Shift+P** opens a search box over everything that can be opened: open
tabs, hosts in the tree (by name, folder, user or address), snippets,
tunnels, the latest commands in the history of the active tab's host, and
actions (local terminal, split view, broadcast, settings…). Type part of the
name (several words, without accents, or some of its letters, skipping
others: "prd" finds "producción"), choose with the arrow keys, and Enter
opens it.

| Shortcut | Action |
|----------|--------|
| `Ctrl+Shift+P` | Command palette |
| `Ctrl+Tab` · `Ctrl+PgDn` | Next tab |
| `Ctrl+Shift+Tab` · `Ctrl+PgUp` | Previous tab |
| `Ctrl+Shift+T` | New local terminal |
| `Ctrl+Shift+W` | Close the tab |
| `Ctrl+Shift+F` | Search in the terminal |

All of them include Shift or a key the shell does not use with Ctrl:
`Ctrl+W`, `Ctrl+T` and the shell's other combinations still reach the
terminal. They can be turned off in **⚙ Settings**, and a snippet cannot use
any of them.

### Safeguards

- **Pasting several lines**: before sending them, the terminal shows what is
  on the clipboard and asks (Enter pastes, Esc cancels). In a shell each
  line runs as it arrives, so a wrong paste can be costly.
- **Closing with live sessions**: when you close a connected tab (or several
  at once, or the application itself with sessions or tunnels running) you
  are asked first. The focus goes to "Cancel": one keypress too many closes
  nothing.
- **Color per folder**: a color is chosen in the settings of a folder (or of
  a host); its tabs show it under the title and in a strip above the
  terminal. Red for production, for example.

Both questions can be turned off in **⚙ Settings**.

### Commands on connect and running on several hosts

- **Commands on connect**: in an SSH host's settings (Advanced) you write
  the commands that should be typed automatically as soon as the shell has
  finished its greeting, also when reconnecting. They accept the snippet
  variables (`{{host}}`, `{{user}}`, `{{name}}`, `{{date}}`…).
- **Run on several hosts**: right-click a folder → **Run a command on its
  hosts…** (or from the palette). You choose the SSH hosts, type the command
  (or start from a snippet) and GodRemote runs it on all of them, a few at a
  time, showing the output, the exit code and the time taken for each one.
  You can cancel, copy the output of one or of all of them, and save it to a
  file. With more than one host, one more confirmation is requested.

### Folders that give values to their hosts

Right-click a folder → **Edit folder…** lets you define, for the hosts it
contains (and those in its subfolders):

- the **user** and the **terminal background**, which apply to the hosts
  that do not have their own;
- the **authentication** (with its password or passphrase in the vault) and
  the **jump hosts**, used by the hosts that select "the one from the
  folder" in their settings (new hosts select it automatically);
- turning off the **info bar** on all of them;
- the **color** their tabs are marked with.

Each host's settings show what it inherits and from which folder. If a
subfolder defines a value, it takes precedence over that of the folder
above.

### Tunnels, jump hosts and X11

In a session's editor:

- **Jump hosts (SSH bastions)**: add, in order, the SSH sessions in the tree
  that have to be gone through. It connects to the first, from it to the
  second, and so on to the destination; the editor shows the resulting path.
  If a jump host in turn needs others, they are inserted automatically.
  Available for SSH, SFTP, Telnet, VNC and built-in RDP (not with the
  external RDP client).
- **X11**: check "Forward graphical applications (X11)" so that the
  graphical applications you launch on that host are shown here, in the
  built-in X server or in the system's (see "Built-in X server").
- **Tunnels**: type, listen address and port, and destination. The terminal
  header shows how many are active. These tunnels live as long as the
  session's terminal does.
- **Forward the SSH agent**: like `ssh -A`. The keys in your agent (OpenSSH
  on Linux, macOS and Windows; on Windows, also Pageant) become available on
  that host so you can jump from it to others with `ssh` or `git`, without
  copying any key.

### More ways to authenticate over SSH

Besides the password and the private key:

- **OpenSSH certificates**: with "Private key", if there is a
  `<key>-cert.pub` next to the key (what `ssh-keygen -s` leaves), the
  certificate is presented to the server, as OpenSSH does. The "certificate"
  field in the editor lets you point to another file and summarizes the one
  that will be used (identity, users it is valid for and validity). If the
  server does not accept it, or it has expired, the key alone is tried, and
  the notice or the error says what happened: expired, belonging to another
  key, the user is not among its principals, or an authority the server does
  not trust. **🔑 Keys** shows, for each certificate, the identity, the
  principals, the validity and the authority that signed it, and marks the
  expired ones in red. Importing or deleting a key takes its certificate
  along. Certificates loaded in the agent are offered too.
- **Second factor (TOTP)**: in the host's editor, "Second factor" accepts
  the TOTP secret in base32 or the `otpauth://…` address from the QR code
  (SHA-1/256/512, 6 to 8 digits, any period). It is stored encrypted in the
  vault, never in the clear. With the vault open, the editor shows the
  current code, how long it has left and a button to copy it. With "Fill in
  the code automatically when connecting" (checked when you type a secret),
  when the server asks for a verification code through keyboard-interactive,
  GodRemote answers it. With the vault locked, or if the server does not
  accept the code, you are asked as usual. It works both when the server
  asks for password and code together (PAM) and in two steps (with
  `AuthenticationMethods` set to `publickey,keyboard-interactive` or
  `password,keyboard-interactive`). The same code is never sent twice: if
  you have just opened another connection to the same host, it waits for the
  next one.
- **SSH agent and Pageant**: "SSH agent" uses the OpenSSH agent and, on
  Windows, also Pageant (PuTTY). **⚙ Settings → Connection → SSH agent**
  (only shown on Windows) chooses between automatic (OpenSSH and, if it is
  not there or has no keys, Pageant), OpenSSH only or Pageant only. Agent
  forwarding (`ssh -A`) uses the same one. **🔑 Keys** lists what is loaded
  in the agent. The Pageant part compiles for Windows, but has not been run
  (see "Known limitations").
- **FIDO2 security keys** (`ed25519-sk`, `ecdsa-sk`): they are used through
  the SSH agent; `ssh-add` loads them and the agent asks the security key
  for the signature. With "SSH agent" there is nothing else to do. You can
  also choose the security key's file as "Private key": GodRemote looks for
  that key in the agent. Before signing, it warns you that you have to touch
  the security key. **🔑 Keys** marks them as "security key".

### Outbound proxy

If the network does not allow going out directly, **⚙ Settings → Outbound
proxy** makes the first connection to each host go through an **HTTP**
(`CONNECT`) or **SOCKS5** proxy, with a user and password if it asks for
them (the password is stored in the vault). It works for SSH, SFTP, Telnet,
VNC and built-in RDP, for the first jump host in a chain and for the tunnel
manager's tunnels.

- Addresses of this very computer (`localhost`, `127.0.0.1`) never go
  through the proxy.
- **No proxy for** accepts comma-separated exceptions: a name, a domain
  suffix (`.internal`) or an address prefix (`10.`).
- Each host can ask for a direct connection in its settings (Advanced).
- The host's name is resolved by the proxy, not by this computer.

### Built-in X server

GodRemote comes with its own X server, written in Rust: there is nothing
else to install or start. In **⚙ Settings → Graphical applications (X11)**
you choose between the **built-in X server** (the default on Windows) and
the **system's** (the default on Linux and macOS: the desktop, VcXsrv,
Xming, XQuartz…).

With the built-in one:

- Connect to an SSH host that has X11 forwarding checked and launch a
  graphical application from its terminal (`xterm &`, `xclock &`…). The
  server starts with the first application and the **X server** tab opens,
  with all the windows inside.
- Each window has its title bar: it is moved by dragging it, resized from
  any edge or corner, **maximized** with its button or by double-clicking
  the bar (and restored the same way), brought to the front with a click and
  closed with its cross. Dialogs appear centered over their window. The
  keyboard goes to the window that has the focus; the mouse, to the one
  under it.
- The **minimize** button on the bar leaves the window as a tile in a strip
  at the bottom of the X screen, and it comes back with a click.
  Applications that minimize or restore themselves are honored too.
  Maximized windows leave room for the strip.
- The **pointer** is the one the application asks for: shape pointers (text,
  hand, wait, resize…) with the system cursor, and image or animated ones
  drawn as they are. Over the window edges the resize pointer appears.
- The screen adjusts itself to the size of the tab, and maximized windows
  follow it.
- The clipboard goes both ways: what you select in an application reaches
  the local clipboard, and the local clipboard is handed to the applications
  when you enter the tab. Inside, you paste as in X: with the middle button
  or, in `xterm`, with `Shift+Insert`.
- Closing the tab **stops the server** and closes the applications it had;
  it starts again by itself with the next one.
- **Also accept programs from this computer** makes it listen on
  `127.0.0.1`; the tab shows the `DISPLAY` to use (for example, from WSL or
  from a local terminal).
- **Each application in its own window** (experimental): in **⚙ Settings →
  Graphical applications (X11) → Applications** you choose "in a tab" (the
  default) or "each in its own window". With the latter, each window of the
  remote applications appears as one more system window, with its title and
  its frame: it is moved, resized and closed like any other, and menus and
  drop-down lists appear in their place. The **X server** tab stays there as
  the control: closing it stops the server and closes the applications. The
  change applies the next time the server starts. It has only been tested on
  Linux with X11; on Wayland it is not available and the applications stay
  in the tab.

The server understands the basic X11 protocol (*core*: windows, drawing,
fonts, colors, keyboard, mouse and selections) and the extensions that
current applications ask for: **RENDER** (compositing with transparency and
anti-aliased text: used by GTK, Xft and Java), **SHAPE** (shaped windows),
**RANDR** (so that they know the size of the screen and when it changes),
**XKEYBOARD** (XKB: the keyboard map and state) and **XInput 2.0** (keyboard
and mouse as devices). With the last two, **GTK 4** applications start.

GTK 4 applications are best launched with `GSK_RENDERER=cairo` (for example,
`GSK_RENDERER=cairo gtk4-demo &`): without it they paint with software
OpenGL, send the whole window on every frame and, when maximizing or
resizing, may leave areas unpainted.

| Applications | Status |
|--------------|--------|
| Classic (`xterm` (also with `-fa`), `xclock`, `xcalc`, `xeyes`, `xlogo`, `xmessage`, `xfontsel`, `xev`) and utilities (`xdpyinfo`, `xprop`, `xwininfo`, `xlsfonts`, `xrandr`, `wmctrl`) | Tested |
| `xdotool` | Finds and inspects windows; cannot type or click (XTEST is missing) |
| **GTK 3** | Tested: keyboard (with Ñ and €), mouse, menus, clipboard both ways |
| **GTK 4** | Tested (`zenity` and `gtk4-widget-factory`; `gtk4-demo`, only by eye): keyboard (with Ñ and €), mouse, pop-up menus and wheel; better with `GSK_RENDERER=cairo` |
| **Qt 5** | Tested: keyboard, mouse, clipboard, and minimizing itself and coming back |
| **Tk**, **Motif** | Tested: buttons, menus, dialogs and typing |
| **Java (Swing)** | Tested: buttons and typing; the text is not anti-aliased |
| Qt 6, browsers (Chromium, Electron, Firefox), LibreOffice | Untested |
| Anything that needs OpenGL (GLX) | Does not work |

For whatever does not work here, the system X server is still available.

Remote applications are not to be trusted, and the server is built with that
in mind: a malformed request cannot bring GodRemote down (at most, the
application that sends it is kicked out) and the memory it can make the
server allocate is bounded.

### Tunnel manager

**🔀 Tunnels**, in the top bar, stores tunnels that do not depend on any
terminal. Each one has a name, the SSH host in the tree it goes through
(with its jump hosts and its authentication, inherited or its own), the type
(`-L`, `-R` or `-D`), where it listens and where it leads.

- **▶ Start** opens an SSH connection just for that tunnel; **⏹ Stop**
  closes it. The button in the bar shows how many are running, in green, or
  in red if any has failed.
- **Start it when GodRemote opens** starts it automatically (after the vault
  is unlocked, so as not to ask for passwords that are already saved).
- If the connection drops, the tunnel comes back up by itself with the same
  waits and the same limit of attempts as the terminals (**⚙ Settings**).
- The tunnels are stored in `tunnels.json` and are included in the backup.

### Server info bar

Under each SSH terminal there is a bar with the server's status. The gear on
the bar itself (or **⚙ Settings → Server info bar**) opens its settings:

- **What is shown, and in what order**: session name, server name, user and
  address, IP addresses, operating system, kernel, uptime, load average, CPU
  usage, memory, swap, free disk space, network traffic, logged-in users,
  processes and server time.
- **Disks**: the paths whose free space you care about (`/, /home, /var`).
- **Update every** so many seconds, small usage bars, and from what
  percentage a value is painted in the warning or error color.

The data is requested over a separate channel of the same SSH connection:
nothing is typed in your terminal and no other connection is opened. The
server needs a POSIX shell (`sh`); CPU, memory and network usage come from
`/proc`, so on systems other than Linux only the name, the system and the
disk appear. On hosts where it does not apply (routers, switches) it is
turned off in the session's settings, under "Terminal: tab color, bar and
background".

### Status dashboard for a folder

Right-click a folder → **📊 Status dashboard** (or, in the palette, "Status
dashboard for the hosts in a folder", which takes the selected folder). A
window opens that watches all the SSH hosts in the folder and its subfolders
at once:

- **What it shows for each host**: connection status, latency, CPU usage,
  load, memory, the fullest disk, uptime, a sparkline of the last few
  minutes (CPU or load) and the time of the latest data. Hovering over a row
  shows the details: load over 1, 5 and 15 minutes, memory, all the disks
  and the error if there is one. What a host does not provide (another Unix,
  a cut-down BusyBox) is left empty.
- **Refresh**: every 10 seconds by default. Each host keeps its connection
  between refreshes; a lightweight query runs over it (it reads `/proc` and
  calls `df`) and latency is measured with an SSH probe. Closing the window
  closes all the connections. When collapsed (the triangle in the title) it
  keeps watching and notifying, but does not repaint.
- **Table**: clicking a column header sorts by it, and clicking again
  reverses the order. The box filters by name, address, status or alert.
  "Only with problems" leaves only what is going wrong. A double-click opens
  the host's terminal; right-click → "Retry now".
- **Connections**: at most 32 open at once (adjustable), and they are opened
  four at a time. If the folder has more hosts than the limit, they take
  turns: each one connects, gives its data and makes way.
- **If a host goes down** it is retried automatically, with waits of 2, 5,
  10, 20, 30, 60 and 120 seconds. There is no further retry if what fails is
  the authentication or the server's key.
- **Credentials**: passwords or passphrases that are not in the vault, and
  the keys of unknown servers, are asked for as in the rest of the
  application when the dashboard is opened or when you press "Retry now". If
  you cancel, the host stays at "credential missing" and the others carry
  on. Automatic reconnections never ask. When the vault locks, the dashboard
  forgets the credentials; when it is unlocked, the hosts that were waiting
  for one are retried automatically.
- **Alerts** ("⚙ Refresh and alerts" button):
  - CPU above 90%, or load above 2 per core, for 3 samples in a row;
  - memory above 90%;
  - fullest disk above 90%;
  - host unreachable (the attempt to connect fails) or without access (a
    credential is missing, or the server rejects it).

  The row is highlighted and the value is shown in red. The application
  notifies you in its status bar, at the bottom of the window, when an alert
  is raised and when it is resolved, not on every sample; those that
  coincide go in a single notice. The history of the last 200 is kept at the
  bottom. Each threshold can be changed or turned off; they are stored in
  `settings.json` (`panel_estado`).

### Themes

By default GodRemote **follows the operating system**: it uses the
"GodRemote Claro" theme if the system is in light mode and "GodRemote
Oscuro" if it is in dark mode, and switches by itself when the system
changes (within a few seconds; up to about twenty if the window is idle).
**🎨 Themes** opens the manager:

- **Follow the system (light or dark)** turns that mode on or off. With it
  on, you choose which theme goes with each mode (any in the list, including
  your own) and you can see which one the system is in.
- Choose a theme from the list and it is applied instantly. When following
  the system, it becomes the theme for the mode the system is in at that
  moment; when not following it, it is the fixed theme.
- **Duplicate and edit** opens the editor, with everything grouped into
  sections. Changes are shown live:
  - **Colors** of the interface and the terminal, including the 16 ANSI
    ones.
  - **Font and line spacing**: font size of the interface, headings, small
    text, buttons, terminal and info bar; line spacing of the interface and
    the terminal, and spacing between the letters of the terminal.
  - **Spacing and shape**: horizontal and vertical spacing between items,
    button padding, minimum height of rows and buttons, rounding, border
    thickness, window and menu margins, tree indentation, scrollbar width
    and terminal margin.
  - **Terminal background** (see below).
- **Download base theme** saves a readable JSON to modify with any text
  editor; **Import a theme** loads it (you can also drop the file on the
  window).

The system mode is read from the registry on Windows ("app mode" under
*Personalization → Colors*), from the preferences on macOS and, on Linux,
from the desktop portal (`org.freedesktop.appearance`) or, failing that,
from the GNOME/GTK or KDE settings. If it cannot be found out (a window
manager without a desktop, for example), the fixed theme is used. Anyone who
had already chosen a theme in an earlier version keeps it: the automatic
mode only turns itself on if they were still using the default one.

A theme is a JSON like this one; any missing fields are taken from the base
theme (the file that "Download base theme" gives you has them all):

```json
{
  "nombre": "Mi tema",
  "oscuro": true,
  "interfaz": { "fondo": "#1b1b1b", "acento": "#d35400", "texto": "#c4c4c4" },
  "terminal": { "fondo": "#1a120b", "texto": "#ffd9a8", "ansi": { "rojo": "#cc6666" } },
  "letra": { "interfaz": 14, "terminal": 16, "interlineado_terminal": 1.2 },
  "forma": { "redondeo": 4, "espaciado": 8, "relleno_botones_vertical": 4 },
  "fondo_terminal": { "tipo": "texto", "texto": "PRODUCCIÓN", "color": "#ff5c5c",
                      "opacidad": 15, "giro": -25, "repetir": true }
}
```

### Terminal background

In the theme editor, **Terminal background** lets you put behind the text:

- a **text** (for example `PRODUCCIÓN`), with its size, color and rotation,
  once or repeated all over the terminal like a watermark;
- an **ASCII drawing**, typed, pasted or loaded from a text file;
- an **image** (PNG, JPEG, GIF, BMP or WebP), at its own size, fitted,
  filling, stretched or tiled.

In all three cases you choose the **opacity** (from 0 to 100%) and the
position. The image is stored inside the theme itself, so it travels with it
when exported; very large ones are scaled down when loaded.

Each host can also have **its own background**, which replaces the theme's:
in the session's settings, "Terminal: tab color, bar and background" →
"Custom background for this host". It is useful for telling sensitive
servers apart at a glance.

### Split view

**⊞ Split**, to the left of the tabs, tiles the terminals: all the open ones
or only the ones you choose, with as many columns as you like. A click on a
terminal gives it the keyboard (it is marked with a frame); a double-click
on its title, or its enlarge button, makes it full screen. With split view
on, the **📡 Broadcast** button moves to the tab bar: it is the convenient
way to type on several servers while seeing what each one answers.

### Search, log and reconnect

- **Search** (`Ctrl+Shift+F` or the 🔍 button): searches the screen and the
  whole scrollback, highlights the matches and jumps between them with Enter
  and Shift+Enter.
- **⏺ Log** saves to a text file whatever the terminal shows, without colors
  or escape sequences. In **⚙ Settings** you can ask for logging to be
  always on, with the time on each line, and choose the folder. To save the
  session with its timing and watch it again, see "Recording and replaying
  sessions".
- **Reconnection**: if a terminal loses its connection, it reconnects by
  itself, with increasingly long waits, keeping what was on screen. There is
  no retry when what fails is the password or the key, or when you close the
  session yourself. It is adjusted (or turned off) in **⚙ Settings**, where
  you can also ask for the tabs from last time to be reopened on startup.

### Links, highlighted words and activity notices

- **Links**: `http://`, `https://`, `ftp://` and `www.…` addresses that
  appear in the terminal are underlined on hover and open in the browser
  with **Ctrl+click**, or with the right button (**Open link** / **Copy
  link**). A link split across two rows is recognized whole.
- **Highlighted words**: `error`, `failed`, `denied`… are shown in red;
  `warning`, `timeout`… in yellow; `ok`, `success`, `done`… in green. In
  **⚙ Settings** you can change the lists, add rules with their own color
  (one from the theme's palette or a custom one) or switch it all off. Only
  whole words count, case-insensitively, and whatever the server has already
  colored is left as it is.
- **Activity**: a terminal tab that receives output while you are looking at
  another one is highlighted; if the bell has also rung, it shows a 🔔. It
  goes away when you open the tab, and can be turned off in **⚙ Settings**.

### Triggers and command-finished notice

**🛠 Tools → Triggers**, in the top bar (or "Triggers" in the palette),
manages rules of the kind "when this text appears in a terminal's output, do
something":

- **Text**: literal or, checking *Wildcards*, with `*` (any text) and `?`
  (one character); `\*` and `\?` are the signs themselves. It is
  case-insensitive unless asked otherwise. "Test with" says whether a sample
  line would fire the rule.
- **Scope**: all terminals (local ones too), a folder or a host.
- **Notify**: a notice in the application's status bar and a ❗ mark on the
  tab if you are not looking at it; *with bell* the mark is 🔔 and, if
  GodRemote is not in the foreground, it asks for attention in the taskbar.
- **Send a text** (answering `y` to a question): it has to be checked and
  confirmed, and the rule is flagged in the list with "⚠ SENDS TEXT". It is
  written like a snippet (a line break is Enter). Every answer is announced.
- **Cooldown**: seconds before the rule can fire again in the same terminal
  (minimum 2 s if it sends text).

Safeguards: what you type or paste, and what a snippet or a trigger sends,
does not fire anything (the line being typed on is not looked at until it
ends, and what a trigger sends is ignored if it reappears within the next
5 s). A rule that sends text only answers the line where the cursor is
waiting, once it has been still for a moment: never what scrolls past in a
listing, nor a full-screen application. Only one rule answers per question,
a rule whose answer contains its own text cannot be saved, and one that
answers more than 6 times in a minute is paused in that tab.

**Long commands**: in the same window, "Notify when a command lasting more
than N s finishes in a tab I am not looking at" (enabled, 30 s). If the
shell marks its commands (OSC 133: fish 4, PowerShell in Windows Terminal,
bash or zsh with the integration of kitty, WezTerm, iTerm2…) the notice is
exact and says whether it ended with an error. Otherwise it is inferred:
Enter was pressed, that much time passed until the last output and then
there is silence with the cursor on something that looks like a prompt or a
question.

### Recording and replaying sessions

- **🎬 Record**, in each terminal's header (also in **🛠 Tools** and in the
  palette), saves the session with its timing as **asciicast v2** (`.cast`,
  the asciinema format). What is recorded is what the terminal shows and its
  size changes, not what is typed. It starts with whatever is on screen,
  goes to the log folder and stops by itself on reaching 1 GB. In
  **⚙ Settings → Session logging** you can ask for all sessions to always be
  recorded.
- **🛠 Tools → Play a recording…** opens a `.cast` of its own or from
  asciinema (versions 2 and 3) in a window; the menu lists the latest
  recordings in the folder. It has play/pause (Space), back to the beginning
  (Home), a position bar (arrow keys: ±5 s), speed from 0.5× to 8× and "Skip
  silences" (no pause lasts more than 2 s, or whatever the recording
  recommends). The terminal is shown at the recorded size, shrinking the
  font if it does not fit. Several can be open at once.

### Command history

GodRemote remembers the commands you run on each host in the tree (local
terminals share one history). The **🕘** button in the terminal header,
**🛠 Tools → Command history** or the palette open a window with a search
box: Enter or a double-click **types** the command in the active terminal
without running it, Shift+Enter (or ▶ Run) runs it, and it can be copied or
deleted. The last 40 of the active tab's host also appear in the palette
("History").

- What is noted down is what the terminal **shows** when Enter is pressed,
  without the shell prompt; never what is typed. A password typed without
  echo is not saved. Neither are answers to questions (lines ending in `:`,
  `?` or `[y/N]`), anything masked with asterisks, or commands that start
  with a space.
- With OSC 133 the prompt is known exactly and only the shell's commands are
  noted down. Without it, the prompt is whatever was in front of the cursor
  when typing began or, as a last resort, it is recognized by its shape
  (`$ `, `# `, `> `…).
- In the same window: stop remembering those of that host, turn off the
  whole history, change the maximum per host (500; from 20 to 5000) and
  delete the history of one host or all of it. The history of a host deleted
  from the tree is forgotten on startup.

### Credential vault

**🔐 Vault** stores passwords, passphrases and TOTP secrets (the
second-factor ones) encrypted with a master password. Besides creating,
unlocking, locking and changing the password:

- **View stored secrets…** lists each secret with its owner (the host or the
  folder) and lets you delete it. To view or copy a value you have to enter
  the master password again. Secrets of hosts that no longer exist are
  marked and can be deleted in one go.
- **Automatic locking**: after 15 minutes with no keyboard or mouse activity
  in GodRemote the vault locks itself. The time is changed (or turned off)
  in **⚙ Settings**.

### Local terminal

**💻 Local terminal** opens the shell chosen in **⚙ Settings → Local
terminal** (by default, the system's). The arrow next to it offers the other
shells found on the computer (PowerShell, cmd, WSL and Git Bash on Windows;
bash, zsh, fish… on Linux and macOS) to open one for a single tab. In the
settings you can also type any command, for example `wsl.exe -d Ubuntu`.

### SFTP: drop, permissions, resume and sync

- **Drag and drop**: drop files or folders from the system's file browser
  onto the SFTP tab and they are uploaded to the remote folder you are
  viewing, with the usual progress and cancellation.
- **Permissions**: the remote list shows the permissions (`rwxr-xr-x`).
  Right-click → **Permissions…** opens the nine checkboxes and the value in
  octal or as text; on a folder it can also be applied to all its contents.
- **If the destination already exists**, you are asked file by file (inside
  folders too): **Resume** continues from where it stopped when the
  destination is smaller than the source, **Replace** or **Skip**, with the
  option of applying it to all.
- **Sync…** compares the local folder with the remote one (with their
  subfolders, by size and date) and shows the **plan** before touching
  anything: what is uploaded, what is downloaded, what is the same and what
  exists on one side only. There are three modes (local → remote, remote →
  local and both ways) and a separate checkbox, unchecked, to **delete**
  from the destination what is extra (only in the one-way modes, with the
  number of files in view). The copies keep the date, so a second pass finds
  nothing to do.

### SFTP: edit and follow the terminal

- **📝** opens the selected remote file with your computer's editor (the one
  associated in the system, or the one you specify in **⚙ Settings**, for
  example `code -w` or `notepad`). Every time you save it, it is uploaded
  automatically; the pane shows the files being edited and when they were
  uploaded.
- An SFTP pane opened from a terminal (**📂 SFTP**) can **follow the
  terminal**: it goes to the folder the shell is in. It needs the shell to
  announce it in the window title (the `bash` of Debian, Ubuntu or Fedora
  does so out of the box) or with the OSC 7 sequence.

### Importing hosts and backup

**⚙ Settings → Import hosts or make a backup…**:

- **OpenSSH**: one host for each `Host` in `~/.ssh/config`, with its
  `HostName`, `User`, `Port`, `IdentityFile` and `ProxyJump` (as jump
  hosts).
- **PuTTY**: the `.reg` file produced by
  `reg export HKCU\Software\SimonTatham\PuTTY\Sessions putty.reg`; on
  Windows, also the sessions saved on the computer, directly.
- **MobaXterm**: the file from "Export all sessions to file" or
  `MobaXterm.ini`; SSH, SFTP, Telnet, RDP and VNC are imported, with their
  folders.

What is imported goes into a new folder in the tree. Passwords are not
imported.

The **backup** saves in a single file the hosts, the vault (encrypted, as it
is), the settings, the snippets, the tunnels of the tunnel manager, the
themes and, if you want, the keys in GodRemote's folder. When it is restored, the current configuration is saved
first, in case you need to go back.

### Syncing the configuration between your computers

**⚙ Settings → Sync between my computers…** (also in the palette and, once
it is active, by clicking the 🔄 indicator in the status bar). Choose a
folder you already replicate between your computers (Dropbox, OneDrive,
Syncthing, a network drive, a repository you manage yourself) and a sync
password, the same on all of them.

- The folder holds a single file, `godremote-sync.json`, encrypted with
  Argon2id + XChaCha20-Poly1305 and compressed: nothing can be read, neither
  host names nor users.
- **Synced**: the hosts and folders, the snippets, the tunnel manager's
  tunnels, the user's themes, the settings and the vault.
- **Not synced**: the private keys (only their path, as text; if it does not
  exist on a computer, this is noted under "Latest notices" and connecting
  gives the usual error), the logs, the recordings, the command history, the
  accepted TLS fingerprints and the settings specific to each machine
  (editor, local shell, log folder, proxy, X server, SSH agent).
- **When**: on startup (as soon as there is a password), a few seconds after
  something changes here or in the folder, every so often (5 minutes by
  default) and with **🔄 Sync now**. Also with the window minimized.
- **How it merges**: three-way, item by item and field by field, with the
  last agreed state as the base. If only one side changed, that side wins;
  two changes in different fields of the same host are both kept; deletions
  propagate and do not come back to life. No timestamp decides anything, so
  clocks that are out of step do not matter.
- **Conflicts**: if both sides changed the same thing differently (or one
  deleted what the other changed), you are asked: **Mine** or **The
  folder's**, one by one or all at once. Until you choose, each side keeps
  its own.
- **Pre-sync backup**: before changes that came from outside are applied on
  this computer, `antes-de-sincronizar-….json` is saved in GodRemote's
  folder (the last 10). They are restored from "Import and backups".
- **The vault** travels as a single block, encrypted with its master
  password: sync never sees a secret. If it changed on one side only, the
  other side receives it. If it changed on both, with the vault open they
  are combined secret by secret (and you are asked about the ones that
  clash); with the vault closed it stays as a conflict until you open it or
  keep one of them whole. If the one in the folder has a different master
  password, it is asked for in order to combine them.
- **The sync password** is stored in the vault; with the vault closed it
  lives only in memory and is asked for.
- **If something goes wrong**: with a half-written file or the folder not
  mounted it retries by itself, without touching anything; with the wrong
  password or a damaged file it says so clearly, without touching anything;
  if two computers write at the same time, the one that loses notices and
  sends its own changes again; and you are told about any "conflicted"
  copies left by the sync service, and they can be deleted.
- **Changing the password or repairing a damaged file**: type the new one
  and press **Rewrite the folder with what this computer has…**; on the other
  computers, type the new one.

### Sharing a folder of hosts with teammates

**⚙ Settings → Folders shared with teammates…** (or the palette).

- **Publish**: choose one of your folders, the destination file (in a
  location your teammates can reach) and a shared password. Its hosts and
  subfolders are published with address, user, key path, jump hosts, notes
  and colors. **Not published**: passwords or anything from the vault,
  commands on connect, tunnels, agent or X11 forwarding, TOTP autofill,
  "bypass the proxy", or serial-port hosts. It is published again with
  **Publish now** or by itself when the folder changes, and it never
  overwrites a file that is not an earlier publication of its own.
- **Subscribe**: file path and password. The folder appears in the tree as
  "👥 Name (shared)", read-only: you can connect, open SFTP, run commands on
  its hosts and duplicate a host (the copy goes outside the folder and is
  now yours), but not edit, move, delete or create anything inside it. With
  **My user** and **My authentication** (private key or agent) you use your
  own on its SSH hosts without touching what is published. It updates itself
  (it is checked every 30 seconds); with no connection the last copy is
  used. **Unfollow…** removes its hosts or leaves them as a folder of your
  own.
- If a shared host disappears, its open tabs keep working (you are told),
  but they are not saved to be reopened on startup. If its address, port or
  protocol is changed, you are told and the password you had saved for it is
  forgotten.
- The folders you follow are not included in your sync. If you follow the
  same one from two of your computers, the saved credentials are valid on
  both.
- The passwords of shared folders are stored in the vault; with the vault
  closed they are asked for.

### Language

The interface is in Spanish and in English. By default it uses the operating
system's language (Spanish if the system is in Spanish, Catalan, Galician,
Basque or Asturian; English in any other case) and it is changed in
**⚙ Settings → Language**, instantly and without restarting. The names of
the built-in themes, the configuration files and whatever is already written
in a terminal or in a log do not change with the language.

### Snippets and shortcuts

A snippet is a named text or series of commands that is sent to the active
terminal. **⚡ Snippets** lists them (with their shortcut next to them) and
lets you:

- **New snippet…**: name, text and shortcut. Each line break is Enter; if
  the text does not end with a line break, it is left typed, waiting for you
  to complete it. `\e`, `\t` or `\x03` stand for special keys.
- **Record by typing in the active terminal**: what you type until you press
  **Stop** is saved as a snippet.
- **Manage snippets and shortcuts…**: edit, launch and delete.

To assign a shortcut, press **Assign…** and then the key combination. Any
combination with Ctrl or Alt is valid, and so are the function keys
(F1–F12), alone or with Shift; a single letter is not valid, nor are Ctrl+C,
Ctrl+V and Ctrl+X, and two snippets cannot share a shortcut. Shortcuts act
when the keyboard is in a connected terminal, and take priority over
whatever that combination means in the shell (which is why Ctrl+Alt+letter,
Ctrl+Shift+letter or the function keys are the best choice: they do not
clash with anything).

The text can use **variables**, which are replaced when it is launched:
`{{host}}`, `{{user}}`, `{{port}}`, `{{name}}`, `{{date}}` and
`{{time}}`. With `{{?Service}}` or `{{?Service=nginx}}` you are asked for a
value (with its default) before it is sent. With broadcast on, each terminal
receives the snippet with its own data. Anything between double braces that
is not a variable (`docker ps --format '{{.Names}}'`) is left as it is.

### Typing in several terminals at once

The **📡 Broadcast** button in each terminal's header opens its options:

- **Type in several terminals at once** turns it on.
- **In all open terminals**, or **only in the ones I choose**, checking them
  in the list.

With broadcast on, what you type, paste or launch with a snippet in one
terminal of the group is also sent to the others: you type `hello` and it
appears in all of them. While you are typing for several, the terminal has a
colored frame, the header says how many you are typing in and their tabs
change color. A terminal that is not in the group keeps working separately.
Broadcast turns itself off when the application is closed.

## Where the data is stored

In the user's configuration directory (`~/.config/godremote` on Linux,
`%APPDATA%\godremote` on Windows; it can be changed with the
`GODREMOTE_HOME` variable):

| File | Contents |
|------|----------|
| `sessions.json` | Tree of hosts. **Contains no secrets.** |
| `vault.json` | Passwords, passphrases and TOTP secrets, encrypted. |
| `keys/` | Keys generated or imported from the application. |
| `themes/` | The user's themes. |
| `settings.json` | Chosen theme and the other settings, including the triggers and those of the status dashboard (`panel_estado`). |
| `state.json` | Tabs open on exit (to reopen them). |
| `version.json` | When it last checked whether there is a new version, and which one it was. |
| `logs/` | Terminal session logs and `.cast` recordings. |
| `command_history.json` | Command history per host. Not included in the backup. |
| `edit/` | Temporary copies of the remote files being edited. |
| `macros.json` | Snippets and their shortcuts. |
| `tunnels.json` | The tunnel manager's tunnels. |
| `sync.json` | Folder, interval and name of this computer for sync. |
| `sync-estado.json` | The last state agreed with the sync folder (the base for the merge) and the pending conflicts. |
| `antes-de-sincronizar-*.json` | Backups taken before applying synced changes (the last 10). |
| `shares.json` | Folders I publish and folders I follow, with the last copy of the latter. |
| `tls_known_hosts.json` | Fingerprints of the accepted RDP certificates. |
| `rdp/` | Logs of the last run of the external RDP client. |

If the master password is forgotten, the secrets in the vault cannot be
recovered; the rest of the configuration remains intact.

## Security

- **The only connection GodRemote makes without being asked** is the
  new-version notice: on startup, and at most once a day, it makes an HTTPS
  request to `github.com` (with no body or user data; through the outbound
  proxy if there is one) and reads from the response the number of the
  latest published version. If it is newer, it says so in the status bar
  with a link to the downloads page; it does not download or install
  anything. It is turned off in **⚙ Settings → Safeguards → Notify when a
  new version is available**. The "hardened SSH" edition does not have it:
  it does not include TLS and makes no connection on its own.
- **Telnet and VNC encrypt nothing**: use them only on trusted networks or
  through an SSH tunnel.
- With the external RDP client the password is passed through standard input
  (FreeRDP 2 and 3), never on the command line.
- Dynamic and local tunnels listen on `127.0.0.1` unless another address is
  given.
- X11 forwarding gives remote applications access to the X server (they can
  read the keyboard and the screen of the other X applications): enable it
  only with trusted servers. With the built-in server that access stays
  among the X applications of that same tab (also when they appear in
  separate windows): it does not reach the rest of your desktop.
- **Agent forwarding**: for as long as the session lasts, whoever
  administers the remote host can use your keys through the agent (not copy
  them). Enable it only on trusted hosts. Agent channels opened by a server
  that was not asked to forward the agent are rejected.
- **Outbound proxy**: the proxy sees which hosts and ports you connect to
  (not the content, which is encrypted in SSH and RDP; in Telnet and VNC it
  would see it). With an HTTP proxy the user and the password travel
  unencrypted as far as the proxy.
- The built-in X server only serves connections that arrive through an SSH
  forwarding and present that session's cookie. If you enable "Also accept
  programs from this computer", it also listens on `127.0.0.1` **without
  authentication**: any program on your computer can connect.
- The links in the terminal are written by the server: only those starting
  with `http`, `https` or `ftp` are opened, and they are handed to the
  browser without going through a command interpreter. Even so, look where a
  link leads before opening it (it is shown on hover).
- A tunnel in the manager set to start automatically opens an SSH connection
  every time you start GodRemote: set it only on trusted hosts.
- **Second factor (TOTP)**: the secret is stored encrypted in the vault,
  never in the clear, and the same code is not sent twice.
- **Triggers that send text**: they answer any server or program that writes
  that text. Do not put passwords in them: they are stored unencrypted in
  `settings.json`.
- **Logs, recordings and command history**: they keep what was seen on
  screen, including secrets typed in plain view (`mysql -pclave`). On Linux
  and macOS they are created readable only by the user (permissions 0600).
- The command history is read from the screen, which is written by the
  server: that is why the normal action is to type the command in the
  terminal without running it.
- A `.cast` from someone else is untrusted input: when it is opened, its
  size, its dimensions and its timings are bounded, and whatever it asks the
  terminal is not answered.
- **Sync and shared folders**: what comes from the sync folder or from a
  shared file is treated as untrusted input even if it decrypts: limits on
  size, number of items and depth, bounded key-derivation costs, and only
  the usual configuration files are written.
- From a shared folder, nothing is accepted that would make GodRemote act on
  someone else's behalf (commands on connect, tunnels, forwardings), nor
  line breaks in single-line texts. The identifiers of its hosts are
  derived: whoever publishes cannot make them match those of your hosts in
  order to take their credentials. A saved password always belongs to a
  host, never to the folder, and a file older than the last one seen is not
  accepted.
- Anyone who has the password of a shared folder and can write to its path
  can replace what is published: give it only to those who should publish or
  read.
- `sync-estado.json`, `shares.json` and the pre-sync backups hold
  configuration in the clear (like `sessions.json`), with private
  permissions; the vault is always encrypted inside them.

## Known limitations

- **Built-in RDP**: tested only against FreeRDP's "shadow" server, not
  against Windows, xrdp or GNOME Remote Desktop. Network Level
  Authentication (NLA) with NTLM, no Kerberos; no RD Gateway, farm
  redirection or smart cards; no audio, drives, printers, multiple monitors
  or live resizing (for a different size you have to reconnect). The text
  clipboard and non-ASCII characters are implemented but not verified
  against a real server. If something fails, the same session can be opened
  with the external client (right-click → "Open with the external client").
- **SSH library version**: the full edition uses russh 0.65.1 instead of
  0.65.4 (see "Two editions"); the "hardened SSH" edition uses 0.65.4 and
  does not come with built-in RDP.
- **Import**: the PuTTY and MobaXterm formats were implemented from their
  documentation and tested with hand-written sample files, not with real
  exports. From MobaXterm, SSH gateways are not imported (you are told, so
  that you can add them as jump hosts), nor are the types GodRemote does not
  have. OpenSSH's `Include` and `Match` are not followed.
- **Folders with values**: changing a folder does not affect tabs that are
  already open, only new connections.
- **Split view**: terminals only (no SFTP or desktops), 16 at most; in the
  tiled layout each terminal has a reduced header.
- **Automatic reconnection**: terminals only; in Telnet a dropped connection
  cannot be told apart from a normal close, and there is no retry.
- **Follow the terminal**: it depends on the shell announcing its folder; in
  `zsh`, `fish` or minimal shells it may need to be configured.
- **Remote editing**: if someone else changes the file on the server while
  you are editing it, saving overwrites it without warning.
- **SFTP**: resuming takes what is already in the destination as good (there
  is no checksum). Normal transfers do not keep the file's date; sync does.
  Sync stops at the first error, does not follow symbolic links or special
  files (it lists them as "not synced") and applies the same mode to files
  and folders in the recursive permission change. Drag and drop has been
  tested on Linux (X11) with a synthetic source, not from a real file
  browser nor on Windows, macOS or Wayland.
- **Fonts**: a theme that asks for a font that is not installed uses the
  built-in one (and says so in the editor).
- VNC: no encryption (VeNCrypt/TLS) and no Tight encodings; the clipboard
  only supports Latin-1 text, as the protocol defines.
- Remote desktops: the Windows key and Ctrl+Alt+Del only from the buttons;
  Ctrl+Alt together is treated as AltGr.
- **Built-in X server**: basic X11 protocol, BIG-REQUESTS, RENDER, SHAPE,
  RANDR (this one only reports: it does not change resolutions), XKEYBOARD
  and XInput 2.0. No XFIXES, MIT-SHM, XTEST, Composite, DAMAGE, SYNC,
  Present/DRI3, XINERAMA or GLX: anything that uses OpenGL does not work,
  and Qt 6, Chromium/Electron, Firefox and LibreOffice remain untested; for
  those, the system X server.
  - From XKB, the keyboard is served (map, names, controls, compatibility
    and state), with a single group; there is no geometry, indicators or
    `GetKbdByName`, and requests that change the keyboard are accepted with
    no effect.
  - From XInput there is only version 2.0, with one pointer and one
    keyboard: no smooth scrolling (the wheel arrives as buttons 4–7),
    touchscreens, gestures, barriers, "raw" events or device properties;
    from XInput 1 there is only the version and the device list.
  - From RENDER, some rarely used pieces are missing (the "disjoint" and
    "conjoint" operators are treated as the basic ones, convolution filters
    as "nearest", and there are no alpha maps).
  - There is no visual with transparency (ARGB), so there are no translucent
    windows, and GTK 4 floating menus appear with a black margin where their
    shadow would go.
  - GTK 4 without `GSK_RENDERER=cairo` is slow and may leave areas unpainted
    when resizing; that comes from Mesa when there is no MIT-SHM. Of GTK 4,
    `zenity` and `gtk4-widget-factory` have an automated test; `gtk4-demo`
    has only been checked by eye.
  - In its tab it works on a single screen, with a simple window manager:
    move, resize, minimize, maximize, bring to front and close. Full screen
    and dragging from GTK's own bars are implemented but untested with real
    applications. Animated cursors only have a unit test: they have not been
    seen on screen.
  - Separate windows are experimental and off by default. They are only
    tested on Linux with X11 (not Windows, not macOS, not HiDPI, not
    multiple monitors): with an automated test, `xterm`; by eye, GTK 3 and
    GTK 4 applications and a `yad` dialog; not with Qt 5, Tk, Motif or Java,
    nor without a window manager, nor through a real SSH connection. On
    Wayland they are not available: the fallback to the tab is implemented,
    but has not been seen on a real Wayland. Dialogs are not tied to their
    window and may end up behind it. Minimizing or maximizing from the
    system frame is not reported to the application. Shaped windows appear
    rectangular. With monitors to the left of or above the main one
    (negative coordinates) the mouse does not reach properly. One X pixel
    equals one interface point, so with HiDPI or zoom they will appear
    scaled. Dragging from GTK's own bars is done by the system and takes a
    few frames to start, and the image cursor is painted in software inside
    the window.
  - Text in the basic protocol is drawn with two built-in fonts (one
    fixed-width and one proportional), whatever X font is requested, so its
    metrics do not match those of a traditional X server to the pixel
    (applications that bring their own glyphs through RENDER, such as GTK,
    Qt or `xterm -fa`, do not notice).
  - In classic Xlib applications (Tk, Motif) only Latin-1 typing has been
    tested: the "€" did not arrive in the test, which was run without a
    UTF-8 locale.
  - Large selections (over 1 MB) are not transferred.
  - `xdotool` finds and inspects windows, but cannot type or click (XTEST is
    missing).
  - A hostile application cannot bring GodRemote down or exhaust its memory,
    but it can keep the X server busy for a while. Performance has only been
    measured locally, not over a real network.
  - None of the new features (cursors, minimizing, XKEYBOARD and XInput,
    separate windows) have been tested on Windows or macOS.
- **SSH agent and Pageant**: with Pageant (PuTTY), authentication, key
  listing and agent forwarding are implemented and compile for Windows, but
  have not been run. The OpenSSH agent, forwarding included, has not been
  tested on Windows either.
- **SSH certificates**: user certificates only; host certificates are still
  not supported. When importing from OpenSSH, `CertificateFile` is not read.
  Folders have no certificate field: with the folder's key, only the
  `-cert.pub` found next to it is used. Certificates are neither generated
  nor signed. Certificates for `-sk` keys have not been tested.
- **Second factor (TOTP)**: time-based codes only, no HOTP or push
  notifications. Autofill depends on the text of the prompt ("verification
  code", "código de verificación", "OTP", "one-time", "token", "passcode"…);
  if the server asks in some other way, you are asked by hand. The clock
  needs to be accurate. There is no QR reader. Any text made of letters is
  valid base32, so a miscopied secret is not always detected when you type
  it. With long periods, if the current code has already been used and the
  next one is more than 45 s away, you are asked by hand. A second step that
  is not password or keyboard-interactive (two keys, GSSAPI) fails with a
  message that says so.
- **FIDO2 keys**: only through the agent; GodRemote does not talk to the
  security key (it does not generate `-sk` keys or ask for the PIN). Tested
  without hardware: with a security key simulated in software and with
  `sk-dummy`, OpenSSH's test provider; never with a physical key (real
  touch, PIN, key unplugged).
- **Outbound proxy**: HTTP `CONNECT` with basic authentication and SOCKS5
  with no authentication or with user and password; no NTLM or Kerberos, no
  chained proxies and no automatic configuration file (PAC). The external
  RDP client does not use it. In Telnet there is no per-host "direct
  connection".
- **Palette and shortcuts**: the shortcuts are fixed (they can be turned
  off, not changed). In a remote desktop GodRemote captures them too: they
  do not reach the remote host.
- **Pasting several lines**: the question appears even if the remote shell
  supports bracketed paste (which already prevents immediate execution).
- **Commands on connect**: SSH only; they are written into the terminal as
  if typed, so they depend on the shell being ready (it waits until the
  greeting stops, or four seconds).
- **Run on several hosts**: without a terminal (like `ssh equipo 'orden'`),
  so it is no use for commands that ask something; it keeps up to 512 KB of
  output per host; if it times out or is cancelled, the command may keep
  running on the server.
- **Status dashboard**:
  - SSH hosts only, and full data only on Linux (`/proc`). A POSIX shell and
    being able to run commands are required; otherwise the host is listed as
    "no data", with its latency.
  - One window (one folder) at a time. Changing folder or reloading resets
    connections, history and charts, which live only in memory. Changes to
    the tree and the connection limit are applied on reload (🔄).
  - Each host opens its own connection and those of its jump hosts: N hosts
    behind the same bastion are N connections to the bastion.
  - With more hosts than allowed connections, each one is updated only when
    its turn comes and reconnects on every turn.
  - Credentials typed without saving them in the vault are not remembered:
    the next reconnection leaves the host at "credential missing".
  - The thresholds are common to all hosts, and load is only judged if the
    number of cores is known.
  - The fullest disk excludes, by type, tmpfs, squashfs, overlay, user FUSE
    and read-only ones, except the root.
  - Latency is that of an end-to-end SSH probe, not a ping.
  - The notices are those of the application's status bar, not system
    notifications.
  - On closing, the remote query ends at its next write, at most one
    interval later.
  - The window needs about 980 points of width.
  - Not verified: Windows (it is only known to compile), real servers that
    are not Linux or that run BusyBox (they have been simulated), a password
    as the credential (it was tested with a passphrase, which uses the same
    prompt), the "the server's key has changed" case, vault locking due to
    inactivity (it was tested by locking it from the palette), folders with
    hundreds of hosts, sessions lasting hours and, from the interface, hosts
    behind a jump host (these are covered in the automated tests).
- **Vault**: locking on inactivity looks at the keyboard and the mouse
  inside GodRemote, not at the system session. Sessions that are already
  open carry on; a reconnection that needs a saved password will ask for it.
- **Tunnel manager**: one tunnel per connection (two tunnels to the same
  host open two SSH connections); it does not count the connections or the
  traffic that go through each tunnel.
- **Links**: only web addresses written as such; hyperlinks with their own
  text (OSC 8) and file paths are not recognized. If the line starts above
  what is visible on screen, the visible part is not recognized as a link.
- **Highlighted words**: the rules are lists of words or phrases, not
  regular expressions, and they are the same for all terminals.
- **Activity notice**: terminal tabs only; a program that writes non-stop
  (`top`, `tail -f`) keeps its tab marked all the time.
- **Triggers**: they work on lines, so they do not see what a full-screen
  application draws. A line on which you press a key stops being watched
  until its line break. With a flood of output only the last 2000 lines of
  each frame are looked at. The "bell" makes no sound: it is the 🔔 mark and
  the request for attention in the taskbar, and the latter could not be
  verified (it was tested without a window manager).
- **Command-finished notice**: without OSC 133 it is a heuristic, which may
  notify about a program that goes quiet after ": " or "> " and does not
  recognize unusual prompts unless the history has already seen them. With
  OSC 133, whatever runs inside a command in progress (another shell opened
  with `sudo` or `ssh` that does not mark its commands) is neither notified
  nor noted down. The minimum threshold is 5 s. It has not been tested with
  shells that come with OSC 133 out of the box (fish, kitty…), only with a
  hand-made bash integration.
- **Recording and player**: what is typed is not recorded; anything that is
  not UTF-8 is replaced. The initial capture does not include the
  scrollback. Going backwards redoes the playback from the beginning, spread
  over several frames; with huge recordings it takes a while. Limits when
  opening: 256 MB, 2 million events and 8 MB lines; pauses longer than an
  hour are shortened to an hour. It does not read asciicast v1. Version 2
  has been cross-checked with asciinema 2.4 in both directions; version 3,
  only with hand-made files following the specification. The system dialog
  for choosing a `.cast` could not be tested (the menu list could).
- **Command history**: without OSC 133 it cannot tell the shell from other
  programs with their own prompt (mysql, python): what is typed into them is
  noted down too. If your shell's prompt ends in `:` or `?`, it is taken for
  a question and nothing is noted down. Only hosts in the tree and local
  terminals; quick connections are not covered. If you jump from a terminal
  to another host with `ssh`, its commands are recorded under the tab's
  host. Of a multi-line paste only the first line is recognized; of a
  multi-line bracketed paste, none. If you type two commands before the echo
  of the first one arrives, neither is noted down. With a right-hand prompt
  (zsh) the command is cut off at 10 consecutive spaces. Commands longer
  than 1000 characters are not saved. From the interface it has only been
  checked with the local terminal; with SSH hosts, in the automated tests.
- Info bar: only in SSH terminals (not in Telnet, serial or the local
  terminal) and designed for Linux servers; on Windows it gives no data. A
  server that limits sessions per connection to one (`MaxSessions 1`) does
  not allow the channel it needs to be opened.
- Terminal background: only the first frame of an animated GIF is used.
- Snippets: shortcuts only act with the keyboard in a terminal (not in a
  remote desktop or in SFTP).
- Broadcast: the keyboard, pasted text and snippets are repeated, not the
  mouse. Each terminal receives the same keys, but what they do depends on
  what it has on screen: it is best for all of them to be at the same point.
- Jump hosts: at most eight chained; each jump host authenticates with its
  own session (there is no SSH agent forwarding between jump hosts).
- No Mosh or FTP.
- **Sync**: tested with local folders standing in for the replicated folder,
  not with real Dropbox, OneDrive, Syncthing or network drives, nor with
  three or more computers at once. It uses a single file that is rewritten
  whole, and the merge is done on the interface thread: with themes or
  backgrounds with large images it may be noticeable. While the host or
  folder editor, or the snippets, themes or tunnels window, is open, what
  has been received waits until it is closed. Conflicts are resolved per
  whole item (mine or the folder's), not field by field. Restoring a backup
  on a synced computer propagates to the others like any other change,
  deletions included (they save their pre-sync backup). Combining vaults
  secret by secret requires the vault to be open.
- **Shared folders**: publications and subscriptions belong to each computer
  (the paths do) and are not synced. Jump hosts that go through hosts
  outside the folder do not work for whoever follows it (both sides are
  told); tunnels and commands on connect do not travel; "my user / my key"
  only apply to SSH/SFTP hosts; a common password for the whole folder
  cannot be set (on purpose). "Stop publishing" does not delete the
  published file. When you unfollow a folder with the vault closed, the
  saved passwords of its hosts are left orphaned in the vault viewer.
- **English**: the translation has been reviewed by eye, window by window,
  on Linux; it has not been reviewed by a native speaker. Error messages
  that come from the operating system or from the libraries (almost always
  in English) are shown as they are in both languages. The screenshots in
  this document are in Spanish.
- **Theme following the system**: checked by hand only on Linux, with the
  GTK/GNOME desktop portal; on Windows and macOS reading the mode is
  implemented and compiles, but has not been seen working. On Linux it is
  queried with the desktop's own tools (`gdbus`, `dbus-send` or `busctl`,
  and `gsettings`): without any of them, or without a desktop session, it is
  not detected and the fixed theme stays. It has not been tested with KDE
  Plasma.
- It has only been used by hand on Linux (X11). On Windows and macOS it is
  built, passes its automated tests on every change (the local terminal one
  among them) and the application is checked to open and paint its window
  (on Windows, on a machine without a graphics card, with DirectX's software
  mode), but the interface has not been operated there: still unchecked by
  hand are, for example, shell detection, dragging files from the file
  browser, agent forwarding on Windows and everything about Pageant. Nor
  have the status dashboard, the triggers, session recording and replay, the
  command history or the new X server features been seen working there.

## Tests

```bash
cargo test
```

```bash
cargo test --manifest-path reforzado/Cargo.toml --target-dir target
```

The second command runs the same tests (except those of the built-in RDP)
with the "hardened SSH" edition, that is, against russh 0.65.4.

- Unit tests for the model and folder inheritance, vault, keys (also
  certificates and security keys), TOTP codes (RFC 6238 vectors, base32 and
  `otpauth://`), terminal (search, reset, working folder), session logging,
  triggers, shell marks (OSC 133) and command end, asciicast recordings
  (also hostile files), command history, themes, fonts, backgrounds, info
  bar, status dashboard (alerts, trend, sorting and filter), snippets
  (shortcuts and variables), broadcast, split view, importers, backup, links
  and highlighted words, Telnet, X11, X server (cursors, minimizing, XKB,
  XInput 2, separate windows and random requests that must not bring it
  down), tunnels, VNC and RDP.
- `tests/auth_methods.rs`: password and keyboard-interactive against an
  in-process SSH server.
- `tests/ssh_integration.rs`: key with passphrase, host verification, shell
  and SFTP against a real `sshd`.
- `tests/ssh_certificates.rs`: OpenSSH certificates, signed with
  `ssh-keygen -s`, against a real `sshd` that trusts an authority
  (`TrustedUserCAKeys`): Ed25519, ECDSA and RSA, key with passphrase,
  certificate in another path, expired, not yet valid, for another user,
  from another authority, a file that is not a certificate and a certificate
  loaded in the agent.
- `tests/totp_autofill.rs`: second factor against an in-process SSH server
  that rejects repeated codes: password and code together, key and code,
  password and then code, no secret, wrong secret, three failures and two
  connections in a row.
- `tests/security_keys.rs`: FIDO2 keys through the agent against a real
  `sshd`, without hardware. One part uses a fake agent with a security key
  simulated in software (including the key that does not sign) and always
  runs; the other uses the real `ssh-agent` with `sk-dummy`, the test
  provider from the OpenSSH sources, and only runs if it is given in
  `GODREMOTE_SK_PROVIDER`.
- `tests/ssh_features.rs`: one and several chained jump hosts (three real
  `sshd`), `-L`/`-R`/`-D` tunnels and X11; Telnet against a real `telnetd`,
  direct and through two jump hosts.
- `tests/tunnel_manager.rs`: tunnel manager against a real `sshd`: local,
  SOCKS through a jump host and remote, without a terminal; stopping and
  starting again, port in use, connection drop and server down.
- `tests/xserver_integration.rs`: the built-in X server against real X
  clients, namely the classic ones (`xterm`, `xclock`, `xeyes`, `xcalc`,
  `xlogo`, `xmessage`, `xev`…) and GTK 3, GTK 4 (`zenity` and
  `gtk4-widget-factory`), Qt 5, Tk, Motif and Java (Swing) applications:
  drawing, RENDER, shaped windows, RANDR, keyboard, mouse, pointer shape,
  clipboard, window manager (move, resize, maximize, minimize and come back,
  also a Qt 5 window that minimizes itself), separate windows (with `xterm`)
  and cookie authentication.
- `tests/agent_and_proxy.rs`: the remote host lists the local agent's keys
  only when forwarding is requested, the listing of what is loaded in the
  agent matches that of `ssh-add -l`, and SSH through an HTTP proxy with its
  exceptions, against a real `sshd`.
- `tests/sftp_features.rs`: permissions (also recursive and without
  following links), resuming uploads and downloads, conflicts, sync in its
  three modes with and without deletion, and that nothing leaves the chosen
  folders, against a real `sshd`.
- `tests/fleet.rs`: one command on several hosts against a real `sshd`:
  output, exit codes, parallel limit, timeout and cancellation.
- `tests/dashboard.rs`: status dashboard against real `sshd` servers
  (several hosts, closed port, going down and coming back, connection limit,
  credentials, servers with no data, jump host).
- `tests/terminal_tools.rs`: triggers, recording and history on a terminal
  connected to a real `sshd`: a command reaches the history and its output
  fires a notice, a trigger answers a question only once, what is typed
  without echo appears neither in the history nor in the `.cast`, the
  recording reproduces the screen and a long command with OSC 133 marks
  notifies with its exit code.
- `tests/sync_devices.rs`: sync with two configuration folders over the same
  shared folder: nothing readable in it and convergence, conflicts that wait
  for an answer, simultaneous writes, a half-written, damaged or foreign
  file, wrong password, missing folder, vault as a block and in conflict,
  password change and pruning of pre-sync backups.
- `tests/share_folders.rs`: publishing and subscribing to a folder of hosts:
  sanitizing hostile files, not overwriting other people's files,
  identifiers and own credentials, updates and coexistence with sync.
- `tests/xserver_ssh.rs`: end to end: an application launched over SSH with
  X11 forwarding (against a real `sshd`) ends up drawn in the built-in X
  server; resizing, stopping and restarting.
- `tests/monitor_integration.rs`: info bar against a real `sshd` (data
  consistent with the machine's own, and the query stops running on the
  server when it is closed).
- `tests/vnc_integration.rs`: VNC client against `x11vnc`, direct and
  through an SSH jump host.
- `tests/rdp_integration.rs`: built-in RDP client against
  `freerdp-shadow-cli3` (NLA and TLS only, certificate trust, passwords,
  mouse and keyboard; direct and through an SSH jump host).

The tests against real servers skip themselves if the required program is
missing (`sshd`, `telnetd`, `socat`, `Xvfb`, `x11vnc`…) or if they are not
run as root.

## Structure

```
src/
  model.rs        tree of folders and sessions
  vault.rs        encrypted vault
  keys.rs         SSH keys and certificates
  totp.rs         TOTP codes for the second factor
  ssh.rs          connection, jump hosts, authentication and shell
  agent.rs        SSH agent (OpenSSH and Pageant) and its forwarding
  tunnels.rs      port forwarding and SOCKS5
  tunnel_manager.rs  saved tunnels, with their own connection
  x11.rs          X11 forwarding
  xserver/        own X server (protocol, drawing, fonts, windows,
                  cursors, XKB, XInput 2 and separate windows)
  xhost.rs        on-demand startup of the built-in X server
  sftp.rs         SFTP client and transfers
  telnet.rs       Telnet client
  serial.rs       serial port console
  desktop.rs      common interface of the remote desktops
  vnc.rs          VNC client
  rdp_client.rs   built-in RDP client
  rdp.rs          launcher for the external RDP client
  trust.rs        trust in TLS certificates
  terminal.rs     terminal emulator and local shell
  theme.rs        themes and settings
  fonts.rs        system fonts
  backdrop.rs     terminal background (text, ASCII, image)
  monitor.rs      server status query
  dashboard.rs    status dashboard: persistent connections, alerts and trend
  session_log.rs  session logging to files
  cast.rs         session recording and playback (asciicast)
  highlight.rs    terminal links and highlighted words
  triggers.rs     triggers: rules on the terminal output
  shell_marks.rs  shell state (OSC 133) and end of commands
  cmd_history.rs  command history per host
  macros.rs       terminal snippets, shortcuts and variables
  palette.rs      command palette search
  fleet.rs        one command on several hosts at once
  shells.rs       available local shells
  proxy.rs        outbound HTTP and SOCKS5 proxy
  import.rs       import from OpenSSH, PuTTY and MobaXterm
  backup.rs       configuration backup
  sync.rs         encrypted sync of the configuration between computers
  share.rs        published and followed folders of hosts
  i18n.rs         interface language; i18n/en/ holds the English tables
  update_check.rs new-version notice
  system_theme.rs light or dark mode of the system
  render.rs       what the window is rendered with (DirectX or OpenGL)
  ui/             interface (egui)
```
