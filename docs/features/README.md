# Feature pages

One page per feature of the daemon (`crates/alpheus-daemon`), plus one for the terminal control
center (`crates/alpheus`), each saying what it does, how to use
it, how to configure it, how it is built, which rules the tests enforce on it and what it costs.
The rules and reasons behind them are in [../decisions.md](../decisions.md); how the pages are
written is in [../STYLE.md](../STYLE.md). To add one, follow [../adding-a-module.md](../adding-a-module.md).

## In the bar

- [The system tray](tray.md): the status icons of running applications, and their menus.
- [The calendar](calendar.md): a month grid that opens under the clock.
- [The media module](media.md): what is playing, and a popup to control the player.
- [The resources popup](resources.md): each figure of the resources module with a graph of its last minutes.
- [The focus timer](timer.md): a countdown, a Pomodoro and a stopwatch as a chip, with a popup.
- [The privacy indicators](privacy.md): shows when the microphone, a camera or the screen is captured, and by which app.
- [The failed-units count](failed.md): `󰀦 N` when systemd has units in the failed state.
- [The power menu](power.md): the ⏻ button and its Lock, Log out, Reboot and Power off menu.

## Panels and overlays

- [The quick settings panel](quick-settings.md): volume, brightness, microphone, do-not-disturb, Wi-Fi, Bluetooth, VPN, Shield and session buttons without opening the control center.
- [Notifications](notifications.md): alpheus's own notification server, with bubbles, a bell, a history panel and do-not-disturb.
- [The on-screen display](osd.md): a pill that shows a volume, microphone or brightness level for a moment.
- [The Tools menu](tools-menu.md): one launcher-style box for "do something to my session".

## Behind the scenes

- [Event hooks](hooks.md): a command of your own, run when something happens in the shell.
- [Shield mode](shield.md): a public-network security mode, switched and shown by alpheus while your system does the root work.
- [Displays](displays.md): the daemon's side of screen layouts, profiles and the "keep these settings?" trial.

## The control center

- [The control center](control-center.md): the terminal program with eight tabs, from Sound to Diag.
