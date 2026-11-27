# Feature pages

One page per feature of the daemon (`crates/alpheus-daemon`), plus one for the terminal control
center (`crates/alpheus`), each saying what it does, how to use
it, how to configure it, how it is built, which rules the tests enforce on it and what it costs.
The rules and reasons behind them are in [../decisions.md](../decisions.md); how the pages are
written is in [../STYLE.md](../STYLE.md). To add one, follow [../adding-a-module.md](../adding-a-module.md).

## In the bar

The modules with no page of their own — the launcher button, the workspaces, the window title, the
volume, the network, Bluetooth, the battery and the clock — are described on the bar's own page.

- [The bar](bar.md): every module, what a click does, how it reads the compositor and the system, and the script verbs that drive it.
- [The system tray](tray.md): the status icons of running applications, and their menus.
- [The calendar](calendar.md): a month grid that opens under the clock.
- [The media module](media.md): what is playing, and a popup to control the player.
- [The resources popup](resources.md): each figure of the resources module with a graph of its last minutes.
- [The focus timer](timer.md): a countdown, a Pomodoro and a stopwatch as a chip, with a popup, and a scheduled shutdown beside them.
- [The privacy indicators](privacy.md): shows when the microphone, a camera or the screen is captured, and by which app.
- [The failed-units count](failed.md): `󰀦 N` when systemd has units in the failed state.
- [The keyboard module](keyboard.md): the keyboard layout and the bind mode, from the compositor, opt-in.
- [The power menu](power.md): the ⏻ button and its Lock, Log out, Reboot and Power off menu.

## Panels and overlays

- [The launcher](launcher.md): apps, windows, commands, a calculator, prefixes, a window switcher, command sources and a dmenu mode.
- [The quick notepad](notes.md): one pad of plain text on a key.
- [Clipboard history](clipboard.md): a text history through data-control, off unless enabled.
- [The quick settings panel](quick-settings.md): volume, brightness, microphone, do-not-disturb, Wi-Fi, Bluetooth, VPN, Shield and session buttons without opening the control center.
- [Notifications](notifications.md): alpheus's own notification server, with bubbles, a bell, a history panel and do-not-disturb.
- [The on-screen display](osd.md): a pill that shows a volume, microphone or brightness level for a moment.
- [The Tools menu](tools-menu.md): one launcher-style box for "do something to my session".
- [Captures](captures.md): screenshots of the screen or an area, copied or saved, a colour picker, an area annotated or read as text (OCR) by an external tool, and screen recording, from a key or the Tools menu.
- [The wallpaper picker](wallpaper.md): a wallpaper picked by name from one folder, your setter restarted with it by its handle, the last choice applied again at start.

## Behind the scenes

- [Themes and bar styles](themes.md): presets or wallpaper colours, light and dark, module colours and the six bar shapes.
- [Event hooks](hooks.md): a command of your own, run when something happens in the shell.
- [Shield mode](shield.md): a public-network security mode, switched and shown by alpheus while your system does the root work.
- [Displays](displays.md): the daemon's side of screen layouts, profiles and the "keep these settings?" trial.

## The control center

- [The control center](control-center.md): the terminal program with eight tabs, from Sound to Diag.
