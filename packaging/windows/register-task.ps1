<#
.SYNOPSIS
    Registers `featherbittorrent serve` as a Windows Scheduled Task, by you, on this machine.

.DESCRIPTION
    The example behind the Windows section of docs/autostart.md. Nothing installs it, and nothing in this
    script starts the server: what it registers runs at boot, which is why it prints how to start the task
    instead of starting it for you.

    Why Task Scheduler rather than sc.exe: featherbittorrent.exe is a foreground program, not a Windows
    service, so `sc.exe create` gives you a service that fails with error 1053 the moment you start it.

    The task runs as you, with your password stored (LogonType Password). That is what lets it reach a
    network share: the logon types that need no stored password are given no network credentials, so
    downloads on \\nas\torrents would fail at boot. Run this elevated, and as the account that should own
    the library, because the task runs as whoever runs it.

    Stopping the task kills the process rather than asking it nicely to stop, so the last save after a
    stop is the periodic one: five minutes of work at most. A stop by Ctrl-C, or SIGTERM on the other
    platforms, is what writes the library down.

.EXAMPLE
    .\register-task.ps1 -Binary C:\Tools\featherbittorrent\featherbittorrent.exe

    In an elevated PowerShell. -TaskName picks a different task name; -LogFile puts serve's log somewhere
    other than %LOCALAPPDATA%\featherbit\featherbittorrent\serve.log. A task of this name is replaced, so
    running this again after moving the binary is enough on its own.

.EXAMPLE
    .\register-task.ps1 -Binary 'C:\Program Files\featherbittorrent\featherbittorrent.exe' -PrintArgument

    Prints the argument the task action gets and exits: no cmdlet that touches Task Scheduler runs, so this
    works from any platform. dev/test-service-examples.sh uses it to check the quoting of a path with spaces.
#>
param(
    [Parameter(Mandatory)] [string] $Binary,
    [string] $TaskName = 'featherbittorrent',
    [string] $LogFile = "$env:LOCALAPPDATA\featherbit\featherbittorrent\serve.log",
    [switch] $PrintArgument
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# cmd.exe and its quotes are the trap here. With /c, when the command line after the switch holds more than
# two quote characters, cmd strips the first and the last one and hands the rest to the parser as it stands.
# Two quoted paths in one command line make four quotes, so the plain form
# '"binary" serve 2>>"log"' arrives as C:\Program Files\...\featherbittorrent.exe" serve 2>>"C:\...\serve.log":
# unquoted at the front, so the space in Program Files splits it there, with stray quotes left inside both
# names. Wrapping the whole command in one extra pair gives cmd the pair it wants to remove and leaves
# '"binary" serve 2>>"log"' intact. The doubled quotes below are literal characters: this is a single-quoted
# string, so {0} and {1} are filled in by -f rather than interpolated by PowerShell.
function Get-TaskArgument {
    param([string] $Binary, [string] $LogFile)
    '/d /c ""{0}" serve 2>>"{1}""' -f $Binary, $LogFile
}

if ($PrintArgument) {
    # What dev/test-service-examples.sh calls on any platform: print the argument and leave. No task
    # scheduler cmdlet runs, so nothing is registered and no Windows account or share is needed.
    Get-TaskArgument -Binary $Binary -LogFile $LogFile
    exit 0
}

# The task starts in System32, so a path typed relative to this folder would be looked up there. Making it
# absolute here is also the check that the file exists at all.
if (-not (Test-Path -LiteralPath $Binary -PathType Leaf)) {
    Write-Error "register-task.ps1: no featherbittorrent binary at $Binary"
}
$Binary = (Resolve-Path -LiteralPath $Binary).Path

# cmd.exe appends serve's stderr to the log file and does not create its folder on the way.
$LogFile = [System.IO.Path]::GetFullPath($LogFile)
$logDirectory = Split-Path -Parent $LogFile
if (-not (Test-Path -LiteralPath $logDirectory)) {
    New-Item -ItemType Directory -Path $logDirectory -Force | Out-Null
}

# The account to run as, spelled the way this machine knows it: DOMAIN\user, MicrosoftAccount:... or a UPN
# depending on how the account was made, so Windows is asked instead of the parts rebuilt from guesswork.
$user = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name

# cmd.exe wants the password as text, and Task Scheduler stores it so the task has network credentials.
# Read-Host keeps it off the console and out of any history file while it is being typed.
$securePassword = Read-Host -AsSecureString "Windows password for $user"
$password = [System.Net.NetworkCredential]::new('', $securePassword).Password

$action = New-ScheduledTaskAction -Execute 'cmd.exe' -Argument (Get-TaskArgument -Binary $Binary -LogFile $LogFile)

# At boot, before anybody logs in: that is the autostart.
$trigger = New-ScheduledTaskTrigger -AtStartup

# No time limit (a server killed an hour into a long seed is worse than no task), and if the machine was
# asleep or the network was not up when the trigger fired, take the next chance rather than skipping it.
$settings = New-ScheduledTaskSettingsSet `
    -ExecutionTimeLimit ([TimeSpan]::Zero) `
    -RestartCount 3 `
    -RestartInterval (New-TimeSpan -Minutes 1) `
    -StartWhenAvailable

# Limited, not Highest: a torrent server needs no admin rights. Password logon for the network reason above.
$principal = New-ScheduledTaskPrincipal -UserId $user -LogonType Password -RunLevel Limited

$task = New-ScheduledTask -Action $action -Trigger $trigger -Settings $settings -Principal $principal

# -Force replaces a task of this name, so running this again after moving the binary is enough on its own.
# Registering is all that happens here: nothing in this script starts the server.
Register-ScheduledTask -InputObject $task -TaskName $TaskName -User $user -Password $password -Force | Out-Null

Write-Host ''
Write-Host "Registered task '$TaskName' to run as $user at every boot, logging to $LogFile."
Write-Host 'It has not started: registering a task does not run it.'
Write-Host ''
Write-Host 'Start it now (and at every boot from here on):'
Write-Host "  Start-ScheduledTask -TaskName $TaskName"
Write-Host 'Stop it. This kills the process, so the last save is the periodic one:'
Write-Host "  Stop-ScheduledTask -TaskName $TaskName"
Write-Host 'Watch what it says while it runs:'
Write-Host "  Get-Content -Wait -LiteralPath $LogFile"
Write-Host 'Remove the task. Your config and library are yours, and this script never touched them:'
Write-Host "  Unregister-ScheduledTask -TaskName $TaskName -Confirm:`$false"
