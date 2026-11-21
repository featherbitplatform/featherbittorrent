# Installing featherbittorrent

Every release puts one artifact per system on the
[releases page](https://github.com/featherbitplatform/featherbittorrent/releases), with a `.sha256` beside
each one. This guide says which file to download, how to check it, where its files land, and what installing
never does: no package installs, enables or starts a service, and none of them creates an account. Running
`serve` at boot is your own choice, and [docs/autostart.md](autostart.md) is the guide to it.

## Which file is yours

Each release ships two builds of the same binary. The full build, `featherbittorrent`, serves the browser
web UI beside the API, and the headless build, `featherbittorrent-headless`, carries no front end. On Linux
both come as packages that install the same `/usr/bin/featherbittorrent`; they declare that they conflict
with and replace each other, so installing one removes the other, and you pick the build you want by which
package you install. The archives and the Windows zips are not packages, so both of a system's archives can
sit on disk at once; use the one you want. The table names the full build's files; the headless build's are
the same with `-headless` after `featherbittorrent`.

| System | The full build's file | The headless build's file |
| --- | --- | --- |
| Debian, Ubuntu or Raspberry Pi OS 64-bit | `featherbittorrent-<version>-linux-x86_64.deb`, or `-linux-aarch64.deb` for a 64-bit ARM machine (a Pi 3, 4, 5 or Zero 2 W on 64-bit Raspberry Pi OS) | `featherbittorrent-headless-<version>-linux-x86_64.deb`, or `-linux-aarch64.deb` |
| Fedora, RHEL, Rocky Linux or AlmaLinux 9 | `featherbittorrent-<version>-linux-x86_64.rpm`, or `-linux-aarch64.rpm` | `featherbittorrent-headless-<version>-linux-x86_64.rpm`, or `-linux-aarch64.rpm` |
| Any other Linux with glibc 2.34 or newer | `featherbittorrent-<version>-x86_64-unknown-linux-gnu.tar.gz`, or the `aarch64-unknown-linux-gnu` one | `featherbittorrent-headless-<version>-x86_64-unknown-linux-gnu.tar.gz`, or the `aarch64-unknown-linux-gnu` one |
| macOS, Apple Silicon or Intel | `featherbittorrent-<version>-universal-apple-darwin.tar.gz`: one binary with both architectures in it | `featherbittorrent-headless-<version>-universal-apple-darwin.tar.gz` |
| Windows, x86-64 or arm64 | `featherbittorrent-<version>-x86_64-pc-windows-msvc.zip`, or the `aarch64-pc-windows-msvc` one | `featherbittorrent-headless-<version>-x86_64-pc-windows-msvc.zip`, or the `aarch64-pc-windows-msvc` one |

On Linux `uname -m` prints `x86_64` or `aarch64`, which is the part of the name to match. Nothing is built
for 32-bit ARM. The `.deb`, the `.rpm` and the Linux `.tar.gz` hold the same binary, built against glibc
2.34 (RHEL 9's), so they run from Ubuntu 22.04 and RHEL 9 up to Debian 13 and Raspberry Pi OS 13; on a system
with an older glibc than that this binary does not start. The web UI the full build serves is described in
[docs/web-ui.md](web-ui.md).

## Check what you downloaded

Download the artifact and its `.sha256` into the same directory, then:

```bash
sha256sum -c featherbittorrent-<version>-linux-x86_64.deb.sha256
```

That prints `featherbittorrent-<version>-linux-x86_64.deb: OK` when the file is byte for byte what was
published. The `.sha256` names its file by name rather than by path, which is why both belong in one
directory. macOS ships `shasum`, and `shasum -a 256 -c` reads the same format whatever the file is called.
Windows has no `sha256sum`, so compare it yourself in PowerShell:

```powershell
$name = 'featherbittorrent-<version>-x86_64-pc-windows-msvc.zip'
# The .sha256 holds the 64-character hash first, then two spaces, then the file name.
$expected = (Get-Content "$name.sha256" -Raw).Substring(0, 64)
(Get-FileHash $name).Hash.ToLowerInvariant() -eq $expected   # True is the answer you want
```

## Debian, Ubuntu and Raspberry Pi OS

```bash
sudo apt install ./featherbittorrent-<version>-linux-x86_64.deb
featherbittorrent --version
```

The `./` in front of the name matters: it says "this file here", not "a package with that name from a
repository". Installing copies files into place and runs nothing afterwards: there is no maintainer script,
so no service is enabled and no process starts.

## Fedora, RHEL and family

```bash
sudo dnf install ./featherbittorrent-<version>-linux-x86_64.rpm
featherbittorrent --version
```

Same story: files are placed, nothing is started, and `rpm -qp --scripts` on the file prints nothing because
it carries no scriptlets.

## Any other Linux, the `.tar.gz`

The archive holds one folder with the binary at its top. Put the binary where your `PATH` already looks and
its companions where your shell and your `man` look:

```bash
tar xzf featherbittorrent-<version>-x86_64-unknown-linux-gnu.tar.gz
cd featherbittorrent-<version>-x86_64-unknown-linux-gnu

mkdir -p ~/.local/bin
install -m 755 featherbittorrent ~/.local/bin/    # add ~/.local/bin to PATH if it is not there
```

`~/.local/bin` is on the `PATH` of a usual desktop Linux login; where it is not, add it to your shell's
startup file (`export PATH="$HOME/.local/bin:$PATH"`). To install for every user instead, use `/usr/local/bin`.

The man pages and completions are generated from the same command definition as `--help`, so they cannot
disagree with the flags. They only work once something reads them:

```bash
# Man pages. Either read them from where the archive put them (MANPATH names a directory for man),
# or copy them into a directory your man already reads, ~/.local/share/man or /usr/local/share/man.
mkdir -p ~/.local/share/man/man1 && cp man/*.1.gz ~/.local/share/man/man1/

# bash: bash-completion reads <data dir>/bash-completion/completions/<command> for you, which on a
# usual system means ~/.local/share/bash-completion/completions/featherbittorrent.
mkdir -p ~/.local/share/bash-completion/completions
cp completions/featherbittorrent.bash ~/.local/share/bash-completion/completions/featherbittorrent

# zsh: it only looks for completions in its fpath, so name the directory in ~/.zshrc before the line
# that calls compinit, then start a new shell.
mkdir -p ~/.zfunc && cp completions/_featherbittorrent ~/.zfunc/
echo 'fpath=(~/.zfunc $fpath)' >>~/.zshrc

# fish: it reads its own completions directory, no configuration needed.
mkdir -p ~/.config/fish/completions
cp completions/featherbittorrent.fish ~/.config/fish/completions/
```

To do the same for every user on the machine, put them in the system-wide version of each directory:
`/usr/local/share/man/man1`, `/usr/local/share/bash-completion/completions/featherbittorrent`, a site-functions
directory your zsh lists in its fpath (`/usr/local/share/zsh/site-functions/_featherbittorrent`), and
`/etc/fish/vendor_completions.d/`. What the `.deb` and the `.rpm` install, in the table below, is the same set
of files under `/usr/share`: those are the directories your shell already searches.

## macOS

The archive holds one folder with a universal binary at its top. `/usr/local/bin` is where the launchd
example in [docs/autostart.md](autostart.md) looks for it:

```bash
tar xzf featherbittorrent-<version>-universal-apple-darwin.tar.gz
cd featherbittorrent-<version>-universal-apple-darwin
sudo install -m 755 featherbittorrent /usr/local/bin/
featherbittorrent --version
```

If Gatekeeper refuses the first run, the binary carries the quarantine attribute a browser download gets,
and macOS drops it once you say so:

```bash
xattr -d com.apple.quarantine /usr/local/bin/featherbittorrent 2>/dev/null || true
```

A binary that came out of an archive unpacked in a shell does not carry that attribute, and the line is
harmless when there is nothing to clear.

The archive also holds `man/` (gzipped pages), `completions/` (bash, zsh, fish and PowerShell) and this
guide beside the binary. Nothing installs them for you:

```bash
# Man pages: read them from the archive, or copy them into a directory your man reads and add it to MANPATH.
MANPATH=$PWD/man man featherbittorrent

# zsh only reads completions from its fpath. Many setups already list /usr/local/share/zsh/site-functions;
# ~/.zfunc plus an fpath line in ~/.zshrc works whatever your setup is.
mkdir -p ~/.zfunc && cp completions/_featherbittorrent ~/.zfunc/
echo 'fpath=(~/.zfunc $fpath)' >>~/.zshrc

# bash-completion reads its own user directory, and fish reads ~/.config/fish/completions.
mkdir -p ~/.local/share/bash-completion/completions ~/.config/fish/completions
cp completions/featherbittorrent.bash ~/.local/share/bash-completion/completions/featherbittorrent
cp completions/featherbittorrent.fish ~/.config/fish/completions/

# PowerShell: dot-source completions/_featherbittorrent.ps1 from your $PROFILE.
```

## Windows

Unzip it wherever you keep tools, and put that folder on your own `PATH`. The zip holds one folder named
`featherbittorrent-<version>-<target>`, with the binary at its top. `C:\Tools` is what
[docs/autostart.md](autostart.md) uses; creating that folder takes an elevated shell, while a folder under your
own profile does not.

```powershell
Expand-Archive featherbittorrent-<version>-x86_64-pc-windows-msvc.zip -DestinationPath C:\Tools
$dir = 'C:\Tools\featherbittorrent-<version>-x86_64-pc-windows-msvc'
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
[Environment]::SetEnvironmentVariable('Path', "$userPath;$dir", 'User')
```

Open a new terminal for the `PATH` change to take effect, then `featherbittorrent.exe --version`. No man pages
ship here (Windows has no `man`), and installing is only copying files: nothing runs, and no service is
registered. For tab completion in PowerShell, dot-source the completion the archive carries from your profile:

```powershell
# Add this line to your profile ($PROFILE holds its path; create the file if it does not exist):
. 'C:\Tools\featherbittorrent-<version>-x86_64-pc-windows-msvc\completions\_featherbittorrent.ps1'
```

## What installing puts where

The `.deb` and the `.rpm` install these files, and nothing else:

| | The `.deb` | The `.rpm` |
| --- | --- | --- |
| The binary | `/usr/bin/featherbittorrent` | `/usr/bin/featherbittorrent` |
| Man pages, one per command (`featherbittorrent.1`, `featherbittorrent-serve.1`, `featherbittorrent-user-set.1`, ...) | `/usr/share/man/man1/`, gzipped | the same |
| bash completion | `/usr/share/bash-completion/completions/featherbittorrent` | the same |
| zsh completion | `/usr/share/zsh/vendor-completions/_featherbittorrent` | `/usr/share/zsh/site-functions/_featherbittorrent` |
| fish completion | `/usr/share/fish/vendor_completions.d/featherbittorrent.fish` | the same |
| Documentation | `README.md`, `NOTICE`, `autostart.md`, `server.md`, `reverse-proxy.md` in `/usr/share/doc/featherbittorrent/` (dpkg keeps the license as its `copyright` file) | `README.md`, `autostart.md`, `server.md`, `reverse-proxy.md` in `/usr/share/doc/featherbittorrent/`, `LICENSE` and `NOTICE` in `/usr/share/licenses/featherbittorrent/` |
| The example systemd units | `/usr/share/doc/featherbittorrent/examples/systemd/system/` and `.../user/` | the same |

Nothing lands in `/etc`, `/lib/systemd`, `/usr/lib/systemd` or `/etc/systemd`: the example units live inside
the documentation folder, where no service manager looks at them. Every archive unpacks to one folder with the
binary at its top beside `LICENSE`, `NOTICE` and `README.md`. The Linux `.tar.gz` and the macOS archive copy the
whole asset directory the binary generates, so each holds `man/` (gzipped) and `completions/` for bash, zsh, fish
and PowerShell; the Windows `.zip` carries only the PowerShell completion, because Windows has no `man`. What
differs is the example each one carries and whether this guide comes along: the Linux archive holds
`docs/autostart.md`, `docs/server.md` and `examples/systemd/` with its system and user units, and no
`docs/install.md`; the macOS archive holds those two docs plus this one, and
`examples/launchd/io.featherbit.featherbittorrent.plist` instead of a systemd unit; the Windows `.zip` holds
`docs\autostart.md`, `docs\server.md`, this guide and `examples\windows\register-task.ps1`.

## What installing never does

- It installs no service and enables none: nothing under `/etc/systemd`, `/usr/lib/systemd`, `/lib/systemd`
  or `/etc/init.d`, no LaunchAgent or LaunchDaemon, no scheduled task, and no maintainer script that could
  run anything at install time.
- It creates no account. A system service runs as an account you create yourself
  ([docs/autostart.md](autostart.md) gives the command).
- It starts nothing: after an install no featherbittorrent process is running unless you started one.
- It writes no `config.yaml` and touches no library. The first run creates its own defaults, in the platform's
  config and data directories or wherever `FBTT_CONFIG_DIR` and `FBTT_DATA_DIR` point.

## Upgrading

Install the newer file the same way you installed this one:

```bash
sudo apt install ./featherbittorrent-<newer>-linux-x86_64.deb    # or dnf install ./...rpm
```

The package manager replaces every file the old package installed. It does not restart anything, because it
started nothing: if you set up a service yourself, restart it after an upgrade (`sudo systemctl restart
featherbittorrent`, or stop and start the launchd agent or the scheduled task the way
[docs/autostart.md](autostart.md) shows). For an archive there is nothing to uninstall: unpack the newer one
and use its binary, clearing the quarantine attribute again if it came through a browser.

## Removing

```bash
sudo apt purge featherbittorrent      # Debian and family
sudo dnf remove featherbittorrent     # Fedora and family
```

removes the files the package installed and nothing else: no service to disable (nothing was enabled), no
account to remove unless you created one, and none of your library. A package owns only the files it
installed, so your config and data stay exactly where they are:

- `~/.config/featherbittorrent`: `config.yaml` and `ui.yaml` (`$XDG_CONFIG_HOME/featherbittorrent` if you set
  that variable)
- `~/.local/share/featherbittorrent`: `session.state`, `library.lock`, and `downloads` under it on a system
  with no Downloads folder (`$XDG_DATA_HOME/featherbittorrent`)

On macOS both directories are `~/Library/Application Support/io.featherbit.featherbittorrent`; on Windows
they are `%APPDATA%\featherbit\featherbittorrent\config` and `%APPDATA%\featherbit\featherbittorrent\data`. A
service or container that sets `FBTT_CONFIG_DIR` and `FBTT_DATA_DIR` keeps its library wherever those say,
usually `/var/lib/featherbittorrent`, so it is those directories that hold what you would want to delete.

Want a clean slate? Stop anything you started, then delete those directories yourself. Purging does not: it
removes package files, and your library was never a package file.

## Running it at boot

Nothing above starts featherbittorrent for you. If you want `serve` running before anyone logs in, or
restarting itself after a crash, [docs/autostart.md](autostart.md) walks through systemd (a system unit and a
user one), launchd and Windows Task Scheduler one command at a time, with the example file for each in
`packaging/` and inside every artifact as documentation.
