# Security notes

alpheus is a desktop shell that runs as your user, inside your session. This
page says what it can do, what it deliberately does not do, and how to report
a problem.

## Commands from configuration run as you, unsandboxed

Several settings are commands that alpheus starts as an argument vector (never
through a shell, except where you write `sh -c` yourself; text you type in the
launcher and run with Enter goes through `sh -c`, as in a terminal):
`[workspaces] command`, `[actions]`, `[quick]` (lock, logout, reboot, power
off, Wi-Fi "More…", the toggles' commands), `launcher.toml`'s `terminal`,
`focus` and every `[[sources]]` `list` and `run`, and `config.toml`'s
`[commands]`. They run with your full privileges and no sandbox, exactly as if
you had typed them. Treat these files like a shell profile: do not paste
configuration from a source you do not trust, and keep them writable by you
only. Applications started from the launcher come from the `.desktop` files and
`$PATH` your own session sees.

Program names are resolved through `PATH`; a hostile directory early in your
`PATH` is a hostile alpheus.

## The control socket

`alpheus-launch` talks to the daemon over `$XDG_RUNTIME_DIR/alpheus-launcher.sock`.
The socket is created with mode 0600 inside the session's runtime directory,
and the daemon refuses a peer whose uid is not its own. Anything running as you
can use it (open the launcher, show a `--dmenu` menu and read what is chosen, set
do-not-disturb); that is the boundary of the session, not something alpheus
tries to defend inside.

## Wi-Fi passwords

When you connect to a new WPA network from the quick panel, the password you
type is kept in a fixed-size heap buffer, passed to `nmcli --ask` on its
standard input, and wiped with volatile writes when the field closes, after
the hand-off to `nmcli`, and on drop. It is **never** in a command line,
a log line or a `Debug` print. One limit is documented rather than hidden:
each typed character also passes through the Wayland keyboard library's
per-key string, which is not wiped. If a connection attempt to a new network
fails, the profile NetworkManager created during the attempt is deleted again
(never when there is any doubt); removing one by hand is done with `nmtui` via
the panel's "More…". `GetSecrets` is never called.

`--dmenu --password` shows dots, prints what you typed to standard output as
you typed it, and is never logged.

## Bluetooth

The quick panel's Bluetooth detail registers alpheus's own pairing agent with
BlueZ (`org.bluez.Agent1`, capability `KeyboardDisplay`) **only while the
detail is open or a pairing is in flight**, and never as the default agent. It
answers only BlueZ's own process and only for the device being paired. It
shows a passkey or PIN for you to type on the device, or asks you to confirm a
displayed code; requests to type a code into alpheus, to authorize a device or
a service are **rejected**. Closing the panel mid-pairing cancels the pairing.
BlueZ's own D-Bus policy still decides what your user may do.

## D-Bus: read-only by construction

The system-bus connection that feeds the bar (NetworkManager, UPower, BlueZ,
power-profiles-daemon) only reads, and calls each service through its owner's
unique name so that D-Bus activation can never start one. A test fails if a
write method appears in those files. The single exemption is the Bluetooth
acting connection, which can only send a pinned list of typed calls to BlueZ
(see [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)). Changes to Wi-Fi and to the
power profile go through `nmcli` and `powerprofilesctl`. The session-bus pieces
(tray, notifications, MPRIS, the colour-scheme portal) have their own allowlists.

alpheus also never writes to the compositor (it reads its socket) and never
handles sleep or screen power.

## Notifications and the tray

The notification server strips markup, caps sizes, merges bursts and rate
limits each application; it never reads image data or follows links, and the
history lives in memory only. Do-not-disturb is a file in
`$XDG_STATE_HOME/alpheus`.

## No network access

alpheus makes no network connection of its own: no HTTP client, no telemetry,
no update check. It talks to local Unix sockets (Wayland, D-Bus, the compositor,
its own control socket) and runs the local commands above.

## Reporting a problem

Please open an issue on the project's repository. For something you consider
sensitive, say so in the title without the details and ask for a private channel
first. Include the version or commit, the compositor, and the daemon's log
lines (standard error, prefixed `alpheus-demon:`), with SSIDs, device names and
paths removed.
