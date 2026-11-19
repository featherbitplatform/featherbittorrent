# Running featherbittorrent at boot

No package in this project starts anything, and none of them enables a service: there are no maintainer
scripts, nothing installed under `/etc/systemd`, nothing under `/Library/LaunchAgents` or
`/Library/LaunchDaemons`, no scheduled task, and no service account. Installing puts files in place and
leaves. What this guide covers is the other half: how to run `featherbittorrent serve` under a service
manager if you want it running before you log in, one command at a time, all of it your choice.

The four example files live in [packaging/](../packaging/README.md) and ship inside every artifact as
documentation, never where a service manager would pick them up on its own:

| You want | The example | Where the artifact carries it |
| --- | --- | --- |
| a systemd system service | [packaging/systemd/system/featherbittorrent.service](../packaging/systemd/system/featherbittorrent.service) | `/usr/share/doc/featherbittorrent/examples/systemd/system/` in the `.deb` and `.rpm`, `examples/systemd/system/` in the Linux `.tar.gz` |
| a systemd user service | [packaging/systemd/user/featherbittorrent.service](../packaging/systemd/user/featherbittorrent.service) | the same two places, under `user/` |
| a launchd agent (macOS) | [packaging/launchd/io.featherbit.featherbittorrent.plist](../packaging/launchd/io.featherbit.featherbittorrent.plist) | `examples/launchd/` in the macOS `.tar.gz` |
| a scheduled task (Windows) | [packaging/windows/register-task.ps1](../packaging/windows/register-task.ps1) | `examples\windows\` in the Windows `.zip` |

This guide itself goes to `/usr/share/doc/featherbittorrent/autostart.md` in the Linux packages and sits
beside `server.md` in the archives.

## What runs, and what stopping it means

`featherbittorrent serve` is a foreground program: it runs until something stops it
([docs/server.md](server.md) is the guide to it). A service manager adds three things and nothing else. It
starts the program at boot. It restarts it when it exits badly, which is what "after a crash" means in
practice: every example here restarts on failure and not after a clean exit, so stopping the service for
real does not have to fight the manager. And it stops it with SIGTERM, which is the same thing Ctrl-C is
in a terminal: the API closes, the last save of the library runs, the lock is released, the process exits
0. The systemd units and the launchd agent give that save 90 seconds (`TimeoutStopSec=90`, `ExitTimeOut`
90) before anything harsher arrives, because a large library on a NAS is exactly the case that takes a
while. Windows Task Scheduler is the exception; its section says what to expect there.

`serve` writes everything it has to say to stderr, one line per event, and `RUST_LOG` decides how much of
it (`info` when the variable is unset). Under systemd that lands in the journal; under launchd and Task
Scheduler it goes to a log file you name.

### Run it once before enabling anything

The setup wizard asks its questions only when there is no `config.yaml`, or the file has no `server:`
section, or its bytes could not be read (docs/server.md). A service has nobody at the keyboard, and every
example here closes stdin for that reason (`StandardInput=null`, `StandardInPath=/dev/null`, cmd.exe with
nothing attached): a run with no config then takes every default instead of waiting. It answers the port
and address questions with `6881` and `127.0.0.1:8090`, leaves logins off, takes the download folder this
library would use anyway (`<data dir>/downloads` on a machine with no Downloads folder), writes all of
that into `config.yaml`, and serves localhost only. That is a working server; it is rarely the one you
wanted when you set a service up at boot.

So start the library by hand once, then enable the service:

1. Run `serve` in a terminal as the same account, with the same directories the service will use. Answer
   the questions. Ctrl-C when it prints `listening on <address>`; that answer is already on disk.
2. Add logins the same way (`user set`, `device add`). A running server reads them when it starts, so
   restart it after a change (docs/server.md).

The commands below show the prefix for each setup. Or skip the wizard entirely and write `config.yaml` by
hand: every field and its default is in [docs/server.md](server.md).

## systemd as a system service (a Raspberry Pi, any Linux server)

This is the setup for a machine that serves the network with nobody logged in. The unit runs as its own
account and keeps its library under `/var/lib/featherbittorrent`, so it does not collide with the directories
of the person who installed it.

**Create the account.** No package creates it, because a package that creates an account is halfway to
installing a service:

```bash
sudo useradd --system --create-home --home-dir /var/lib/featherbittorrent \
  --shell /usr/sbin/nologin featherbittorrent
```

`--home-dir` is the parent of both directories the service will use, so it exists and belongs to that
account before anything writes into it. `StateDirectory=featherbittorrent` in the unit covers the same
ground on a machine where you would rather not think about it.

**Copy the unit** from
[packaging/systemd/system/featherbittorrent.service](../packaging/systemd/system/featherbittorrent.service),
which is also `/usr/share/doc/featherbittorrent/examples/systemd/system/featherbittorrent.service` once
the `.deb` or `.rpm` is installed:

```bash
sudo cp /usr/share/doc/featherbittorrent/examples/systemd/system/featherbittorrent.service \
  /etc/systemd/system/featherbittorrent.service
```

**Run it once by hand**, with the two directories the unit names. Both variables are needed: each one
replaces one directory, and naming only the config directory would leave the library somewhere else.

```bash
sudo -u featherbittorrent \
  env FBTT_CONFIG_DIR=/var/lib/featherbittorrent/config \
      FBTT_DATA_DIR=/var/lib/featherbittorrent/library \
  featherbittorrent serve
```

Ctrl-C ends it and saves. Logins come next with the same prefix:

```bash
sudo -u featherbittorrent env FBTT_CONFIG_DIR=/var/lib/featherbittorrent/config \
      FBTT_DATA_DIR=/var/lib/featherbittorrent/library featherbittorrent user set you
```

A device key is a public key, so copy the one from the machine that will connect and pipe it in (the
redirect happens in your shell, which can read your own `~/.ssh`):

```bash
scp laptop:~/.ssh/id_ed25519.pub /tmp/laptop.pub
sudo -u featherbittorrent env FBTT_CONFIG_DIR=/var/lib/featherbittorrent/config \
      FBTT_DATA_DIR=/var/lib/featherbittorrent/library \
  featherbittorrent device add laptop < /tmp/laptop.pub && rm /tmp/laptop.pub
```

**Enable it.** This is the step nothing else does for you:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now featherbittorrent
```

**Watch it:**

```bash
journalctl -u featherbittorrent -f
```

The first line a run prints is `listening on <address>`, which is what to look for after a start.

**Stop, disable, undo:**

```bash
sudo systemctl stop featherbittorrent          # SIGTERM: the last save runs
sudo systemctl disable featherbittorrent       # forget it at boot
sudo rm /etc/systemd/system/featherbittorrent.service && sudo systemctl daemon-reload
```

Removing the unit leaves `/var/lib/featherbittorrent` and everything in it alone, and so does removing the
package: your library is not a package file. The account stays too until you remove it
(`sudo userdel featherbittorrent`).

**After an upgrade**, install the newer package and then restart, because no maintainer script does it:

```bash
sudo apt install ./featherbittorrent-<version>-linux-aarch64.deb   # or dnf install ./...rpm
sudo systemctl restart featherbittorrent
```

## systemd as a user service (a desktop or a laptop)

This one runs as you, in your session, on your own library. It sets no account and names no directories,
and that is the point: it uses the same config and data directories your TUI uses, so a `featherbittorrent`
you open later finds the library held by the running server and offers to connect to it instead of starting
a second engine (the hand-off in [docs/server.md](server.md)).

```bash
mkdir -p ~/.config/systemd/user
cp /usr/share/doc/featherbittorrent/examples/systemd/user/featherbittorrent.service \
   ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now featherbittorrent
```

Without a package installed, the same file is `packaging/systemd/user/featherbittorrent.service` in a
checkout and `examples/systemd/user/` inside the Linux `.tar.gz`.

`--user` throughout: no sudo, and nothing is enabled for anybody else. To keep the server running after you
log out, ask the login manager to keep your user's services up:

```bash
loginctl enable-linger "$USER"
```

Undo with `systemctl --user disable --now featherbittorrent`, and `loginctl disable-linger "$USER"` if you
enabled it. Logs are in the journal too: `journalctl --user -u featherbittorrent -f`.

Because this service shares your directories, run the first-run step as yourself with no variables at all:
`featherbittorrent serve`, answer, Ctrl-C. If a TUI already has the library open, close it first: one
library runs one engine at a time (exit 3 otherwise).

## The Raspberry Pi and the NAS

The reference host for this project is a **Raspberry Pi 2 Model B v1.2** running 64-bit Raspberry Pi OS:
1 GB of RAM, and 100 Mbit Ethernet on a bus shared with USB (spec section 4). Downloads usually live on
the NAS. This section is the system service above with the three things a NAS adds: mount it first, give
the service permission to write there, and keep the cache small.

**Install:**

```bash
sudo apt install ./featherbittorrent-<version>-linux-aarch64.deb
```

**Mount the share before the service starts.** An NFS export in `/etc/fstab`:

```
nas.example.invalid:/volume1/torrents  /mnt/nas/torrents  nfs  _netdev,nofail,x-systemd.automount  0 0
```

or an SMB one:

```
//nas.example.invalid/torrents  /mnt/nas/torrents  cifs  credentials=/etc/featherbittorrent-smb,uid=featherbittorrent,gid=featherbittorrent,file_mode=0664,dir_mode=0775,_netdev,nofail,x-systemd.automount  0 0
```

`_netdev` keeps the mount off the boot sequence until the network is up. `nofail` lets the Pi boot when the
NAS is asleep instead of sitting in a rescue shell. `x-systemd.automount` makes the mount point appear at
boot and wait for the share on first use, so a folder that is not there yet is a slow first access rather
than a failed start. Run `sudo systemctl daemon-reload` (and `sudo mount -a`, or reboot) after editing
fstab.

SMB needs a credentials file. Make it with the right ownership and mode before anything is written into it,
so it is never readable by the rest of the machine even briefly:

```bash
sudo install -m 600 /dev/null /etc/featherbittorrent-smb
sudo ${EDITOR:-vi} /etc/featherbittorrent-smb
```

`install` creates it as root with mode 600; editing an existing file in place keeps that mode. What goes in
it:

```
username=bitbackup
password=whatevertheisset
domain=WORKGROUP
```

`mount.cifs` reads it as root during the mount, so nobody else needs access to it.

The `uid`, `gid` and mode options decide who the mounted files belong to, so name the account the service
runs as or its downloads are unwritable. On NFS the same permission is settled on the NAS: the exported
folder has to be writable by `featherbittorrent`'s uid (or by whoever `root_squash` maps it to). New files
land readable by the share's other users, because the example unit sets `UMask=0022`, which is what lets a
media server on the LAN read them; the library under `/var/lib/featherbittorrent` stays 0700, so
`config.yaml` and its password hash do not.

**Tell the unit about the mount.** In `/etc/systemd/system/featherbittorrent.service`, uncomment both
commented lines and name your mount point, so the `[Service]` section says:

```ini
RequiresMountsFor=/mnt/nas/torrents
ReadWritePaths=/var/lib/featherbittorrent
ReadWritePaths=/mnt/nas/torrents
```

`RequiresMountsFor` is what makes the service wait for the mount instead of starting beside an empty folder.
The second `ReadWritePaths` is needed because `ProtectSystem=strict` makes the whole filesystem read-only
apart from the paths a unit names, and a download folder on a NAS is not inside `/var/lib/featherbittorrent`.

**Configure it.** With the wizard run by hand as in the section above, or by writing
`/var/lib/featherbittorrent/config/config.yaml`:

```yaml
download_dir: /mnt/nas/torrents/downloads   # on the mount, so absolute
listen_port: 6881
cache_size_mib: 16                          # 1 GB of RAM, 100 Mbit shared with USB
server:
  bind: 0.0.0.0:8090                        # away from loopback, so logins are on by default
```

A non-loopback bind turns logins on unless the file says otherwise (docs/server.md), so add a user and the
laptop's device key with the `sudo -u featherbittorrent env ...` prefix shown above before you enable the
service. Then open, on the LAN, port 6881 for TCP **and** UDP (peers arrive over both, on the one port this
product listens on) and port 8090 for the API. On the laptop, run `featherbittorrent`, press `c`, and give
it `http://<the pi>:8090`: sign in with the user or the device key. From outside the LAN the same address
belongs behind a reverse proxy or a gateway, which is [docs/reverse-proxy.md](reverse-proxy.md).

Network filesystems are part of what this project has to work on, not a special case: sparse files may be
refused there, and a rename within one torrent's folder may turn out to cross devices. The engine is built
against that (spec section 4), so the setup above is the whole story on the client side. If your NAS makes
one of those visible as an error in the log, that is worth reporting rather than working around.

## launchd (macOS)

A launchd **agent** runs in your own login session, as you, which means it uses your config and data
directories: the TUI you open finds the server holding the library and offers to connect, exactly as with
the systemd user service. It stops when you log out; for a Mac that has to serve with nobody logged in,
use the daemon variant at the end of this section.

**Install the binary** from the universal archive into `/usr/local/bin`, which is where the example plist
looks for it (change its first argument if you put it somewhere else):

```bash
tar xzf featherbittorrent-<version>-universal-apple-darwin.tar.gz
sudo cp featherbittorrent-<version>-universal-apple-darwin/featherbittorrent /usr/local/bin/
xattr -d com.apple.quarantine /usr/local/bin/featherbittorrent 2>/dev/null || true
```

That last line clears the "downloaded from the internet" flag a browser leaves on; a copy from a terminal
or an unpacked archive does not carry it, and `xattr` reports an error when there is nothing to clear.

**Run it once in a terminal** as yourself (`/usr/local/bin/featherbittorrent serve`, then Ctrl-C) so the
wizard questions are answered before launchd ever starts the process with a closed stdin.

**Install the agent:**

```bash
mkdir -p ~/Library/LaunchAgents
cp examples/launchd/io.featherbit.featherbittorrent.plist ~/Library/LaunchAgents/
sed -i '' "s|/Users/YOU|$HOME|" ~/Library/LaunchAgents/io.featherbit.featherbittorrent.plist
```

The two `/Users/YOU` paths are where the log goes; `sed` replaces them with your home directory. The plist
is otherwise complete: `RunAtLoad` starts it when the agent loads, `KeepAlive` with `SuccessfulExit` false
restarts it after a crash and not after a clean stop, `ExitTimeOut` gives SIGTERM 90 seconds for the last
save, `StandardInPath=/dev/null` closes stdin, and `ProcessType=Background` asks macOS for background
scheduling.

**Load it.** This is the step no installer does:

```bash
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/io.featherbit.featherbittorrent.plist
```

Because `RunAtLoad` is true, loading starts it now. What it says goes to
`~/Library/Logs/featherbittorrent.log`. To ask launchd about the job itself:

```bash
launchctl print gui/$(id -u)/io.featherbit.featherbittorrent
```

**Stop and remove it** with one command (`bootout` stops the process, waits up to `ExitTimeOut` for the last
save, then forgets the agent):

```bash
launchctl bootout gui/$(id -u)/io.featherbit.featherbittorrent
```

To leave the file where it is and still keep it from loading at the next login:

```bash
launchctl disable gui/$(id -u)/io.featherbit.featherbittorrent   # and enable to undo it
```

Deleting the plist is what takes it out of the picture for good.

**A machine nobody logs in to** wants a **LaunchDaemon**: the same plist in `/Library/LaunchDaemons`, plus a
`UserName` key naming the account to run as (a daemon runs as root without one), and an
`EnvironmentVariables` dict so its library is somewhere that account can write:

```xml
<key>UserName</key>
<string>you</string>
<key>EnvironmentVariables</key>
<dict>
    <key>FBTT_CONFIG_DIR</key>
    <string>/Users/you/Library/Application Support/io.featherbit.featherbittorrent</string>
    <key>FBTT_DATA_DIR</key>
    <string>/Users/you/Library/Application Support/io.featherbit.featherbittorrent</string>
</dict>
```

Name an account that exists and can write those two directories: with `UserName` set the daemon runs as
that account rather than as root, so its home and its permissions are what apply to the library. launchd
refuses a daemon plist that somebody else owns or that group or others can write, so it has to be root's own
file before it will load:

```bash
sudo chown root:wheel /Library/LaunchDaemons/io.featherbit.featherbittorrent.plist
sudo chmod 644 /Library/LaunchDaemons/io.featherbit.featherbittorrent.plist
```

An agent in `~/Library/LaunchAgents` must belong to you and not be writable by anybody else, which a file you
copied there yourself already is. Load the daemon with `sudo launchctl bootstrap system
/Library/LaunchDaemons/io.featherbit.featherbittorrent.plist` and stop it with `sudo launchctl bootout
system/io.featherbit.featherbittorrent`. A daemon lives outside any login session, so it cannot reach the
login keychain. For `serve` that changes nothing: its logins are a password hash and public keys in
`config.yaml`, and nothing it does needs a keychain (the TUI is what reads the keychain, and only for a
connect password). The TUI belongs in an agent, not a daemon.

## Windows

**Why not `sc.exe`:** `featherbittorrent.exe` is a foreground program, not a Windows service. `sc.exe
create` will happily register one, and starting it fails with error 1053 because the process never answers
the service control protocol. A native service mode would be a code change, and this milestone does not
make one. What works today is Task Scheduler, which runs an ordinary program at boot.

**Register the task** from [packaging/windows/register-task.ps1](../packaging/windows/register-task.ps1),
which the `.zip` carries as `examples\windows\register-task.ps1`, in an **elevated** PowerShell:

```powershell
cd C:\Tools\featherbittorrent
.\register-task.ps1 -Binary C:\Tools\featherbittorrent\featherbittorrent.exe
```

It asks for your Windows password because the task runs at boot with nobody logged in, and a task that has
no stored credentials is given no network identity: downloads on `\\nas\torrents` would fail. Registering
does not start anything; the script prints what to do next instead. Run it as the account that should own
the library, since that is who the task runs as.

Run `.\featherbittorrent.exe serve` in a terminal once first, for the same reason as everywhere else, and
Ctrl-C when it says `listening on <address>`. Its log goes where the task sends stderr, by default
`%LOCALAPPDATA%\featherbit\featherbittorrent\serve.log`.

**Start, stop, remove:**

```powershell
Start-ScheduledTask    -TaskName featherbittorrent
Stop-ScheduledTask     -TaskName featherbittorrent
Unregister-ScheduledTask -TaskName featherbittorrent -Confirm:$false
```

Stopping a scheduled task ends the process: Task Scheduler sends nothing this program would recognize, so a
stop through the task scheduler has no last save. The newest state on disk is then the periodic one, five
minutes old at worst. Ctrl-C in a terminal does write the library down, and unregistering a task that is not
running loses nothing.

**Network storage must be a UNC path.** Put `download_dir: \\nas\torrents\downloads` in the config, not a
mapped drive letter: a mapped drive belongs to an interactive logon and this task starts before you have
one, so `Z:\` would simply not exist. The library itself stays in your own `%APPDATA%` folders, which the
task can reach because it runs as you.

**The other route** is a wrapper that turns a console program into a real Windows service, so services.msc
and `sc.exe` can manage it. If you go that way, read what your wrapper does when a service is stopped before
relying on it: whether the stop reaches this program as something it understands decides whether the last
save happens at all.

## Where the config lives in each setup

`FBTT_CONFIG_DIR` and `FBTT_DATA_DIR` each replace one of the two directories, and setting both means no
home directory is needed at all. The examples use them for a system service and a LaunchDaemon, where the
account has no desktop session to fall back on, and leave them unnamed everywhere else so the service and
the TUI share one library.

| Setup | Config directory (`config.yaml`, `ui.yaml`) | Data directory (`session.state`, `library.lock`) |
| --- | --- | --- |
| systemd system service (the example unit) | `/var/lib/featherbittorrent/config`, from `Environment=` in the unit | `/var/lib/featherbittorrent/library`, from `Environment=` |
| systemd user service | your own: `~/.config/featherbittorrent` | your own: `~/.local/share/featherbittorrent` |
| launchd agent | `~/Library/Application Support/io.featherbit.featherbittorrent` | the same folder (macOS keeps both there) |
| LaunchDaemon variant | wherever `EnvironmentVariables` says, or that account's own | wherever `EnvironmentVariables` says |
| Windows scheduled task | `%APPDATA%\featherbit\featherbittorrent\config` | `%APPDATA%\featherbit\featherbittorrent\data` |

Two things worth remembering about whichever one you pick:

- A running server **rewrites** `config.yaml`: a device added from a laptop's devices screen is written
  into it immediately. So the account the service runs as has to own the file, which is why the system
  example keeps its library under `/var/lib` and not in `/etc`.
- Nothing removes these directories. Uninstalling a package leaves them, `apt-get purge` included; a clean
  slate means deleting them yourself, once the service is stopped and disabled.
