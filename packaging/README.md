# Packaging examples

These are examples of running `featherbittorrent serve` under a service manager, one file per manager: a
systemd system unit and user unit, a launchd agent, and a PowerShell script that registers a Windows task.

Nothing installs or enables any of them. No package carries a maintainer script, nothing lands in a unit or
init or LaunchAgents directory, no account is created, and nothing starts when you install.

[docs/autostart.md](../docs/autostart.md) is the guide: what each file needs first, the commands that copy
it where a service manager will see it, and how to stop and undo it again.
