# hp-charge-limit-rs

Battery charge limiter for Linux using the firmware's ACPI WMI method
`\_SB.WMID.SBCC` through `acpi_call`.

This branch is Linux-only and tuned for the HP Victus 16-e0xxx (board
`88ED`, BIOS F.24). See [AGENTS.md](AGENTS.md) for the machine-specific
firmware findings and safety rules. The upstream project was tested on an
HP Victus 16-r1000; this branch is maintained for the 88ED machine.

Compatibility is not guaranteed on other PCs: your firmware must expose the
same methods and buffer layout. There is no model or board check, so verify
compatibility before using it; unsupported firmware calls can be unsafe
(see [Is it safe for my laptop?](#is-it-safe-for-my-laptop)).

| mode | SBCC on 88ED | behavior | survives shutdown |
|---|---|---|---|
| `auto` | `0 0 0 0` | charges normally | — |
| `inhibit` | `0x63 2 0 0` (hold) | no charge or slow charge; LED/status may still say charging | yes |
| `hold` | `0x63 2 0 0` | same as inhibit on this board | yes |

On 88ED, `inhibit` and `hold` both use the hold command; true inhibit
(`SBCC 0 1 0 0`) is rejected while charging and is not used. Only auto and
hold are ever written; force-discharge and ship mode are never sent. On
some boards hold only slows charging: see [Tested models](#tested-models).

## Contents

- [Is it safe for my laptop?](#is-it-safe-for-my-laptop)
- [Why this repo?](#why-this-repo)
- [Tested models](#tested-models)
- [How it works](#how-it-works)
- [Install](#install)
  - [Linux (manual installation)](#linux-manual-installation)
  - [Arch Linux (AUR)](#arch-linux-aur)
- [Use](#use)
  - [Setup diagnostics](#setup-diagnostics)
  - [Desktop notifications](#desktop-notifications)
  - [Shell completion](#shell-completion)
  - [Temporary full-charge bypass](#temporary-full-charge-bypass)
- [Daemon options](#daemon-options)
- [Publishing releases](#publishing-releases)
- [Code layout](#code-layout)
- [Verification and limitations](#verification-and-limitations)
- [License](#license)

## Is it safe for my laptop?

The worst this tool can do is stop your battery from charging. Use
`bypass on` to allow normal charging while the service is running, or
`set auto` if no daemon is running. It is still an unofficial tool,
provided without warranty (see the licenses). These are the reasons the
risk is small:

- **It only switches charging on and off.** The three modes are the
  firmware's own charge switches: four flags in the embedded controller's
  (EC) memory, WACL, SBIS, SBTC and WPBS. `auto` clears all four, which is
  the stock state. Nothing touches BIOS settings, NVRAM or flash.
- **It never charges more, and never drains or disconnects the battery.**
  A limiter only ever charges less. The battery pack's own protection
  electronics are not touched; inhibit and hold are states the EC firmware
  itself implements. Force-discharge (0x0200) and battery ship mode
  (0x0600/0x0700, which disconnects the battery) cannot be expressed in the
  code: no command or option sends them.
- **Firmware without this method refuses the call.** `diagnose` only reads.
  `acpi_call` answers `AE_NOT_FOUND` when the method is missing; nothing
  changes. The remaining risk is firmware that has the method but means
  something else by it. That is why you should run
  [`diagnose`](#setup-diagnostics) first, and why reports with your model
  matter.
- **Undo is one command, and stopping does it for you.** With the service
  running, [`bypass on`](#temporary-full-charge-bypass) pauses its limits and
  allows normal charging on the next AC-powered poll. `set auto` is only a
  one-time firmware change: a running daemon can override it. Stopping or
  uninstalling the service sets auto by default. True inhibit ends by itself
  at shutdown; on 88ED the code uses hold for both modes, so the limit
  outlasts a shutdown until `set auto` or the service's next start. In hold
  the LED and battery status may say charging; that is only cosmetic. On
  some boards hold still lets a slow charge through (about 2 W), so it
  limits less, but it never charges faster than normal.

Uninstalling leaves the laptop as it was before: see the uninstall steps
for [Linux](#linux-manual-installation) and [Arch](#arch-linux-aur).

## Why this repo?

I know other repos already expose `0x0500` (inhibit), but none of the ones
I tried stopped my laptop from charging while it was off. They worked
while running, but after shutdown it kept charging.

This branch uses the hold command (`0x63 2`) for both the running limit and
the shutdown limit on 88ED, because true inhibit cannot be entered while
charging there. It also has a low-battery override so the laptop can charge
while off when needed. On this machine hold blocks charging through shutdown
and hibernate. It does not block charging on every board, though: on some it
only slows charging to about 2 W, so a long shutdown still ends near full.
The running limit works on all boards tested so far.

## Tested models

Reports from Linux users, with what each mode did. "Slow charge" means the
firmware accepted hold (readback `hold (3)`) but the battery still charged
at about 2 W, against tens of watts in auto.

| model | board | BIOS | inhibit (running) | hold (off) |
|---|---|---|---|---|
| Victus 16-r1xxx | 8C99 | F.16 | works | works (blocks charging) |
| Victus 16-e0xxx | 88ED | F.24 | works | works (blocks charging) |

The 88ED row is this branch's machine. There, the `inhibit` mode is
implemented as hold (`GBCC 3`); the true-inhibit command is not used.

Your model is missing or behaves differently? Please
[open an issue](https://github.com/kralonur/hp-charge-limit-rs/issues)
with the output of `diagnose`.

## How it works

Default policy with bypass disabled (mode changes depend on successful firmware writes):

```mermaid
flowchart TD
    Start[Daemon starts] --> Capacity{Battery at or below start threshold?}
    Capacity -->|Yes| Auto[Auto: 0 0 0 0]
    Capacity -->|No| Limit[Run mode: hold on 88ED]
    Auto -->|On AC, battery reaches stop threshold| Limit
    Limit -->|On AC, battery falls to start threshold| Auto
    Auto --> Exit{Daemon exits}
    Limit --> Exit
    Exit -->|Service stop| Stock[Auto: normal charging]
    Exit -->|Shutdown or reboot| Low{Battery at or below off-min threshold?}
    Low -->|Yes| ChargeOff[Auto: charge while off]
    Low -->|No| Hold[Off mode: hold on 88ED]
```

Charging resumes at the start threshold (default: 40%) and stops at the
stop threshold (default: 50%). At shutdown, the off-min threshold
(default: 40%) selects normal charging instead of hold when the battery
is low. All three thresholds are configurable.

Between thresholds, the current state is kept. Polls on battery leave the
firmware mode unchanged. Shutdown detection and off-state behavior have
the limitations described below.

## Install

Requires Linux, systemd for the service, and the `acpi_call` module built for
your running kernel. Install the module using your distribution's packages;
DKMS packages also need matching kernel headers. Firmware access requires
root. Some module packages, including Arch's `acpi_call`, already configure
loading at boot; no additional module-load file is needed in that case.

Power supplies are discovered under `/sys/class/power_supply` by type,
ignoring peripherals marked `scope=Device`. One present battery is
selected: `BAT1` if available, otherwise the first battery in alphabetical
name order. Multiple batteries are not aggregated. Any online system-level
mains, USB, or wireless supply counts as external power. Discovery is refreshed
on each read to handle hotplug. Discovery does not establish firmware compatibility.

### Linux (manual installation)

**Prebuilt binary:** once a release is published, download its `.tar.gz`
and matching `.tar.gz.sha256` from this repository's GitHub Releases page.
Choose `x86_64-unknown-linux-musl` for x86_64 PCs or
`aarch64-unknown-linux-musl` for ARM64 Linux. These are static binaries;
you still need the kernel module and compatible firmware.

For example, for an x86_64 release (set `name` to the archive you downloaded,
without `.tar.gz`):

```sh
name=hp-charge-limit-rs-vX.Y.Z-x86_64-unknown-linux-musl
sha256sum --check "$name.tar.gz.sha256"
tar -xzf "$name.tar.gz"
cd "$name"
binary=./hp-charge-limit-rs
```

**Or build from source:** from a checkout, with Rust 1.89 or newer:

```sh
cargo build --locked --release
binary=target/release/hp-charge-limit-rs
```

For either option, check the binary before installing:

```sh
sudo modprobe acpi_call
sudo "$binary" status
```

The binary does not install itself. From the extracted archive or source
checkout, install it, the systemd service, and the
[desktop notification](#desktop-notifications) bus policy and autostart entry:

```sh
sudo install -Dm755 "$binary" /usr/bin/hp-charge-limit-rs
sudo install -Dm644 packaging/hp-charge-limit-rs.service /etc/systemd/system/hp-charge-limit-rs.service
sudo install -Dm644 packaging/io.github.kralonur.HpChargeLimit.conf /usr/share/dbus-1/system.d/io.github.kralonur.HpChargeLimit.conf
sudo install -Dm644 packaging/io.github.kralonur.HpChargeLimit.Notify.desktop /etc/xdg/autostart/io.github.kralonur.HpChargeLimit.Notify.desktop
sudo install -Dm644 README.md /usr/share/doc/hp-charge-limit-rs/README.md
sudo install -Dm644 LICENSE-MIT /usr/share/licenses/hp-charge-limit-rs/LICENSE-MIT
sudo install -Dm644 LICENSE-APACHE /usr/share/licenses/hp-charge-limit-rs/LICENSE-APACHE
# First install only: do not overwrite an existing configuration.
sudo install -Dm644 packaging/hp-charge-limit-rs.conf /etc/hp-charge-limit-rs.conf
sudo systemctl daemon-reload
sudo systemctl enable --now hp-charge-limit-rs
```

Only if your module package does **not** already configure loading at boot:

```sh
printf 'acpi_call\n' | sudo tee /etc/modules-load.d/hp-charge-limit-rs.conf
```

On upgrades, replace the binary and unit but keep your existing config.
Settings: edit `/etc/hp-charge-limit-rs.conf`, then `sudo systemctl restart hp-charge-limit-rs`.
The service creates its state directory automatically.

To uninstall a manual installation:

```sh
sudo systemctl disable --now hp-charge-limit-rs
sudo hp-charge-limit-rs set auto   # stock charging, whatever the stop mode was
sudo rm /usr/bin/hp-charge-limit-rs /etc/systemd/system/hp-charge-limit-rs.service
sudo rm -f /usr/share/dbus-1/system.d/io.github.kralonur.HpChargeLimit.conf \
    /etc/xdg/autostart/io.github.kralonur.HpChargeLimit.Notify.desktop
sudo systemctl daemon-reload
# Only if you created this file during installation:
sudo rm -f /etc/modules-load.d/hp-charge-limit-rs.conf
```

Your config, documentation, licenses, and off-state history remain; to
remove those too:

```sh
sudo rm -rf /etc/hp-charge-limit-rs.conf /var/lib/hp-charge-limit-rs \
    /usr/share/doc/hp-charge-limit-rs /usr/share/licenses/hp-charge-limit-rs
```

You can also remove the `acpi_call` module package if nothing else uses it.

### Arch Linux (AUR)

The AUR packages track upstream, not this 88ED branch. For this branch,
build from this checkout; the packages below are listed for upstream users.
Both depend on `acpi_call`; choose a module package appropriate for your
kernel.

- [`hp-charge-limit-rs-bin`](https://aur.archlinux.org/packages/hp-charge-limit-rs-bin):
  prebuilt static binary from the GitHub Release, for x86_64 and ARM64. No Rust
  or compilation required.
- [`hp-charge-limit-rs`](https://aur.archlinux.org/packages/hp-charge-limit-rs):
  built from source with `cargo`, for x86_64.

Install either one with an AUR helper, for example:

```sh
paru -S hp-charge-limit-rs-bin
```

The two packages provide the same binary and conflict with each other;
install only one. After either installation:

```sh
sudo modprobe acpi_call
sudo systemctl enable --now hp-charge-limit-rs
```

Arch's `acpi_call` package already installs
`/usr/lib/modules-load.d/acpi_call.conf` to load the module at boot.
`modprobe` above loads it for the current session if needed; do not add a
duplicate config. If you use a different module package, check its setup.
The package installs the binary, unit, configuration, licenses, shell
completions, D-Bus policy, and notifier autostart entry;
`/etc/hp-charge-limit-rs.conf` is preserved on upgrades. Edit it and restart the
service to change settings.

To uninstall:

```sh
sudo systemctl disable --now hp-charge-limit-rs
sudo hp-charge-limit-rs set auto        # stock charging, whatever the stop mode was
sudo pacman -R hp-charge-limit-rs-bin   # prebuilt package
# Or: sudo pacman -R hp-charge-limit-rs # source-built package
sudo rm -rf /var/lib/hp-charge-limit-rs # off-state history
```

If you had edited the config, pacman keeps it as
`/etc/hp-charge-limit-rs.conf.pacsave`; delete that too for a clean slate.

## Use

```sh
sudo hp-charge-limit-rs status --watch 60   # energy over 60 s: charging or not
sudo hp-charge-limit-rs set hold            # one-off mode change
sudo hp-charge-limit-rs set auto            # restore normal charging
journalctl -u hp-charge-limit-rs -b         # daemon log for this boot
```

`set` makes a one-time firmware change; it does not pause or reconfigure the
daemon. A running service or foreground daemon can overwrite it on a later
poll, even after `set auto` reports success. To allow a full charge while
the daemon is running, use [`bypass on`](#temporary-full-charge-bypass);
use `bypass off` to resume limiting.
Stop the service first if you want manual control.

Status also shows battery manufacturer/model, health
(full capacity relative to design capacity), cycle count, and power in watts.
Missing readings are shown as `?`. Power uses `power_now`, or current × voltage
when available; health supports both energy- and charge-based sysfs values.
Power measures battery power flow, not total laptop consumption; zero is normal
when the adapter powers the laptop and the battery is neither charging nor
discharging. Cycle count is firmware-reported: a zero may mean an unused battery
or an unimplemented/inaccurate firmware reading. Health is a capacity estimate,
not an independent battery diagnostic.

### Setup diagnostics

```sh
sudo hp-charge-limit-rs diagnose
```

Reports the machine (vendor, model, board ID, BIOS version) for compatibility
reports, then checks battery discovery and capacity, external power supplies,
the `acpi_call` interface and access permissions, and firmware mode readback.
Reports service state and systemd unit/configuration paths; it does not
validate configuration values. An offline adapter is normal. Missing required
supplies, invalid readings, access failures, or failed firmware queries give a
nonzero exit status. An inactive/missing service or unavailable systemd is only
a warning, since manual commands do not require the service.

No charge modes are set, modules loaded, or services changed. Firmware readback
still issues a GBCC query through `/proc/acpi/call` (requiring write access),
not an SBCC mode-setting call. Running without sudo can report permission
failures. Successful diagnostics do not prove firmware compatibility or actual
charge blocking.

### Desktop notifications

The daemon runs as root, so it cannot show notifications itself. Instead it
broadcasts D-Bus signals on the **system bus**, and a small helper running as
your desktop user turns them into standard freedesktop notifications. KDE
Plasma, GNOME and other desktops with a notification server display them.

| signal | when | notification |
|---|---|---|
| `LimitReached(y capacity)` | charging blocked at the stop threshold | Charge limit reached |
| `ChargingResumed(y capacity)` | charging allowed again at the start threshold | Charging resumed |
| `BypassChanged(b enabled)` | bypass took effect or ended | Charge limit paused / resumed |
| `WriteFailed(s mode, s error)` | a firmware write failed (once per run of failures) | Charge control failed (critical) |

Like other desktop helpers for system services (KDE Discover's update
notifier, nm-applet, earlyoom's systembus-notify), the notifier is an XDG
autostart entry: it starts with every graphical login (KDE, GNOME, XFCE and
others), and never in SSH or TTY sessions. Both installation methods above
install the bus policy and the entry. When upgrading a running system,
restart the daemon once so it can claim its bus name, then log out and back
in (or run `hp-charge-limit-rs notify &` for now):

```sh
sudo systemctl restart hp-charge-limit-rs
```

The entry runs `hp-charge-limit-rs` from `PATH`. You can also just run
`hp-charge-limit-rs notify` in a terminal as your user, without root.

To turn notifications off:

- For yourself only, hide the autostart entry the standard way, which also
  survives package upgrades:

  ```sh
  mkdir -p ~/.config/autostart
  printf '[Desktop Entry]\nHidden=true\n' > ~/.config/autostart/io.github.kralonur.HpChargeLimit.Notify.desktop
  ```

- For everyone, stop the daemon sending signals: set
  `HP_CHARGE_LIMIT_RS_NOTIFY=false` in `/etc/hp-charge-limit-rs.conf` and
  restart the service. Running notifiers then stay idle.

Behavior and limits:

- Signals only, no D-Bus methods: nothing on the bus can change charging.
  Only root may own the bus name `io.github.kralonur.HpChargeLimit` (see the
  policy file), and the notifier accepts signals only from that name's
  current owner, including signals sent directly to it, so other users cannot
  fake notifications.
- Best effort. If the bus or the policy is missing, the daemon logs
  `D-Bus: signals disabled` once and keeps limiting normally. It retries at
  most once a minute, and only when it has an event to send. Events raised
  during an outage are dropped, not queued.
- Events come from the limiter's state changes, which only happen on AC power.
  Repeated failures while the firmware keeps refusing produce one
  notification, not one per poll. A new notification replaces the previous one
  instead of stacking.
- Starting, shutting down and stopping the service do not notify, except that
  a failed write at startup is reported. Every user running the notifier sees
  the notifications.
- Watch the raw signals with `busctl --system monitor io.github.kralonur.HpChargeLimit`.
- The D-Bus library accounts for most of the binary size (about 5 MB
  unstripped in a release build) and of the locked dependencies.

What was verified: against private system and session buses with the bus's
default policy, a fake notification server and the real daemon on fake sysfs/ACPI
files (event delivery, replacement, failure deduplication, no bus, name
denied, spoofed senders, malformed payloads, escaped markup, missing
notification server, bus loss, reconnection after the back-off, clean exit on
SIGTERM). The static x86_64 musl build was checked the same way. Not verified:
a real KDE or GNOME shell, the root-owned name on a real system bus (tests used
the current user in the policy instead of root), and the aarch64 build.

### Shell completion

Both AUR packages install completion files for Bash, Zsh, Fish, Elvish, and
PowerShell. Bash needs the `bash-completion` package installed and enabled for
automatic loading. Zsh needs completion enabled (`autoload -Uz compinit &&
compinit` if your shell setup does not already do it). Restart your shell after
installation. Elvish and PowerShell need the generated script loaded explicitly;
installing the file alone does not activate it.

For manual installations, generate scripts without root:

```sh
hp-charge-limit-rs completions bash > hp-charge-limit-rs.bash
hp-charge-limit-rs completions zsh > _hp-charge-limit-rs
hp-charge-limit-rs completions fish > hp-charge-limit-rs.fish
hp-charge-limit-rs completions elvish > hp-charge-limit-rs.elv
hp-charge-limit-rs completions powershell > hp-charge-limit-rs.ps1
```

Load the Bash script with `source hp-charge-limit-rs.bash`, or, with
`bash-completion` enabled, install it in
`/usr/share/bash-completion/completions/hp-charge-limit-rs`. For Zsh, install
`_hp-charge-limit-rs` in a directory on `$fpath`, such as
`/usr/share/zsh/site-functions/`. For Fish, place the script in
`~/.config/fish/completions/`. Elvish uses `eval (slurp < hp-charge-limit-rs.elv)`;
PowerShell uses `. ./hp-charge-limit-rs.ps1` (add it to your profile to persist).

### Temporary full-charge bypass

Use bypass, not `set auto`, when you want a full charge while the service
or a foreground daemon is running. Bypass pauses the daemon's charge limits;
`set auto` only changes the firmware once and can be overridden by the daemon.

```sh
sudo hp-charge-limit-rs bypass on    # pause thresholds and allow normal charging
sudo hp-charge-limit-rs bypass off   # resume the configured charge band
```

The daemon must be running. Changes take effect on its next AC-powered poll;
`status` shows whether bypass is requested, not confirmation that firmware
accepted it.
While enabled, the daemon requests auto instead of enforcing thresholds,
including when it exits at shutdown or service stop. Turning bypass off
reapplies the startup policy on the next AC-powered poll: auto at or below
the start threshold, otherwise the configured run mode.

The flag lives at `/run/hp-charge-limit-rs.bypass`, so a service restart keeps
it but reboot clears it. Bypass allows a full charge while powered off too;
after boot, normal limiting resumes. While on AC, failed bypass/resume writes
are retried on subsequent polls. Shutdown and service-stop writes are attempted
once; failures are logged.

## Daemon options

Each is a `daemon` flag or the environment variable in brackets
(the conf file sets these; a flag wins over the variable).

- `--stop` / `--start` (`HP_CHARGE_LIMIT_RS_STOP` / `_START`): stop and resume thresholds
  while running (defaults: stop 50%, start 40%)
- `--run-mode inhibit` (`_RUN_MODE`): mode used at `--stop` (`hold` also works, orange LED)
  - On 88ED both `inhibit` and `hold` use the hold command.
- `--off-mode hold` (`_OFF_MODE`): mode set at shutdown/reboot (`inhibit`, `auto`, `keep`)
- `--off-min` (`_OFF_MIN`): at shutdown, at or below this threshold use auto
  to charge while off (default: 40%)
- `--stop-mode auto` (`_STOP_MODE`): mode set when only the service is stopped (`systemctl stop`);
  with auto a stopped service leaves the laptop stock, including while off
- `--interval 30` (`_INTERVAL`): poll interval in seconds (minimum 5)
- `--notify true` (`_NOTIFY`): broadcast D-Bus signals for desktop
  notifications; `false` keeps the daemon off the bus entirely

Percentages must be within 0–100, with `start < stop` and `stop >= 1`.
`--run-mode` accepts `auto`, `inhibit`, or `hold`; both exit modes also accept
`keep`. With `--off-mode keep`, the low-battery override does not change the
mode. The config file is read by the systemd service, not by direct CLI runs.

At start it requests auto at or below `start`, otherwise the run mode,
replacing a hold left from shutdown. While running, it stops charging at
`stop` and resumes at `start`; ticks on battery leave the mode alone, and AC
returning re-applies the current mode.
Shutdown is told apart from a service stop by `systemctl is-system-running` = `stopping`.
At shutdown it saves energy_now; at the next start it logs
`off N min in hold: 51% -> 51%, energy +0 mWh` (journal, and in `status`).

## Publishing releases

The release workflow uses current stable Rust and builds static
x86_64 and ARM64 Linux binaries. Archives include the binary, README, licenses,
systemd unit, default config, D-Bus policy, and notifier autostart entry, with
a separate SHA256 checksum.

Set the version in `Cargo.toml`, update `Cargo.lock`, commit, then push a
matching version tag to the fork. This branch is published as
`victussy-88ed`; the local `fork` remote tracks it:

```sh
version=$(cargo pkgid | cut -d'#' -f2)
git tag "v$version"
git push fork main:victussy-88ed
git push fork "v$version"
```

The upstream AUR packages track upstream releases, not this fork.

A pre-release version such as `0.4.0-beta.1` is published as a GitHub
prerelease, which never becomes the repository's latest release.

The tag must match the package version. GitHub Actions publishes the
release and assets using its built-in token; no personal token is needed.
The workflow can also be run manually to build downloadable Actions
artifacts without publishing a release. An existing release must be
removed before rerunning its publishing job.

## Code layout

`main.rs` only parses the CLI and calls `hp_charge_limit_rs::run` (`lib.rs`).

| module | role |
|---|---|
| `cli` | clap definitions, defaults as named constants |
| `daemon/` | `mod.rs` loop: waits on a control channel with the poll interval as timeout; `limiter` the charge band state machine and exit policy; `linux` signal handling |
| `firmware/` | `Mode`/`Reported`, `ChargeControl` trait (implemented by `Wmi`); `acpi_call` SBCC command building, GBCC parsing, transport (lock, single read) |
| `power/` | `Energy`/`Voltage` unit types; `sysfs` battery/AC readings |
| `offstate` | energy record across shutdown |
| `bypass` | temporary bypass flag shared by the CLI and daemon (file under `/run`) |
| `diagnose` | machine/board identification, setup checks, GBCC readback, service configuration reporting; no mode changes |
| `events/` | limiter events; `dbus` signal definitions and the daemon-side emitter (best effort) |
| `notify` | user-side helper turning those signals into desktop notifications |
| `service/` | `systemd` queries |
| `status`, `paths` | status command, system paths |

During normal polling, a failed threshold transition keeps the current state
and is retried on the next AC-powered poll if the threshold condition still
holds. If a startup write fails, initialization is retried on each AC-powered
poll, including between thresholds. Each retry uses the current capacity and
bypass request to select the mode. After a successful write, normal hysteresis
resumes. Stable states are not rewritten on every poll: the daemon re-applies
the current mode when AC returns, and checks GBCC every five minutes, writing
again only if the firmware reports a different mode. Successful writes mean
the ACPI call returned without an `acpi_call` error, not independent
verification of charge flow. `finish` consumes the limiter, so nothing ticks
after it.

## Verification and limitations

`cargo test --locked` runs the limiter unit tests; it does not verify firmware
behavior. `cargo check --locked` checks compilation. On laptops it has only
the user reports in [Tested models](#tested-models).

On this branch's 88ED machine, hold reads back as `GBCC 3` and blocks
charging through shutdown; a successful hibernate also resumed with hold
active. Upstream's 8C99 machine reads inhibit back as GBCC mode 4; weeks of
daily use show shutdown detection and hold working: every shutdown in the
journal logs `shutdown: cap=..% -> hold`, and the next boot reports
`off N min in hold` with no charge gained, over offs of up to 14 hours.
Testers on other boards found hold only slowing charging (about 2 W), so
this is not a guarantee for other machines or firmware versions; see
[Tested models](#tested-models). To check your own laptop, look in the
journal for those two lines.

Still unverified:

- Holding through AC unplug/replug while off.

The off-state record and last report live under `/var/lib/hp-charge-limit-rs/`.
In hold mode, LED and battery status may still say charging; use
`status --watch 300` to observe energy changes instead (a slow charge of
about 2 W needs a few minutes to show).

## License

This project's original code is dual-licensed under either
[MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option
(`MIT OR Apache-2.0`).
