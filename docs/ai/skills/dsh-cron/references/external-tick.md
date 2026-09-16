# Installing the external tick

A dogesh session that is open drives its own cron jobs automatically - nothing else to
install. To have jobs fire while **no session is open** (overnight, after logout), an
external tick has to call `cron tick` on a schedule. `cron setup` prints the exact text
to install; it never installs anything itself, so review it before pasting it in.

Print the form for your platform:

```sh
cron setup --crontab    # a plain crontab line, either OS
cron setup --systemd    # a systemd --user service + timer, Linux
cron setup --launchd    # a launchd agent plist, macOS
```

With no flag, `cron setup` guesses from the running OS (`--launchd` on macOS, the
crontab+systemd text on Linux).

## Linux: crontab

The simplest option, and works even where systemd user services are not set up:

```sh
crontab -e
# then paste the line cron setup --crontab printed, e.g.:
# * * * * * /path/to/dogesh -c "cron tick" >/dev/null 2>&1
```

Verify it is actually firing: check `cron status` a few minutes later, or watch
`/var/log/syslog` (or `journalctl -u cron`) for the `CRON` entries.

## Linux: systemd user timer

Two files, then one command:

```sh
# ~/.config/systemd/user/dogesh-cron.service and dogesh-cron.timer,
# exact contents from: cron setup --systemd
systemctl --user enable --now dogesh-cron.timer
loginctl enable-linger "$(whoami)"   # so it keeps running without a login session
```

Verify: `systemctl --user list-timers dogesh-cron.timer` shows a `NEXT` time in the near
future, and `systemctl --user status dogesh-cron.service` shows successful past runs.

## macOS: launchd

```sh
# Save the plist cron setup --launchd printed to
# ~/Library/LaunchAgents/dev.doge-shell.cron.plist
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/dev.doge-shell.cron.plist
```

Verify: `launchctl print gui/$(id -u)/dev.doge-shell.cron` shows the job as loaded, and
`cron status` shows a recent `last tick`.

## macOS: crontab (also available)

`cron setup --crontab` works unchanged on macOS too, if launchd's own login-session
restrictions are inconvenient - `crontab -e` there behaves the same as on Linux.

## After installing either one

`cron status` reports whether a tick is actually arriving and, if not, a reminder to run
`cron setup`. If jobs still are not firing once a tick is confirmed installed, move to
[troubleshooting.md](troubleshooting.md).
