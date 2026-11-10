# AGENTS.md

Machine-specific findings and operating notes for the HP Victus charge-limit
work on **Victussy**. This file is scoped to this machine; do not generalize
the board quirks below to other HP models without testing.

Last verified: **2026-11-10**.

## Host profile

- Hostname: `Victussy`
- Model: HP Victus by HP Laptop 16-e0xxx
- Board: `88ED` (board version `80.74`)
- BIOS: AMI `F.24`, dated `07/30/2025`
- OS: NixOS `26.11` (`26.11.20260905.dirty`), kernel `7.2.3`
- Battery: HP Primary, Li-ion, 55.178 Wh design/full, 228 cycles, 100% health
- RAM: 14 GiB
- Swap: 4 GiB partition `/dev/nvme0n1p2`, UUID
  `971759d6-3a74-4f7d-824f-7566651474ae`
- Root: ext4 `/dev/nvme0n1p3`, 471.9 GiB
- `/sys/power/state`: `freeze mem disk`
- `/sys/power/resume`: `259:2`
- No `resume=` kernel parameter and no `boot.resumeDevice`; hibernate resume
  works through systemd's EFI `HibernateLocation` mechanism.
- DSDT reference: `/tmp/dsdt.dsl`; `GBCC` at line 8352, `SBCC` at line 8416.

## Non-negotiable safety rules

- **Never send force-discharge commands.** On this firmware those are
  `SBCC 0x63 1 0 0` and `SBCC 0 2 0 0`.
- Never send ship mode commands (`0x0600`/`0x0700`) or anything that
  disconnects the battery.
- Use `hold` for both `Mode::Inhibit` and `Mode::Hold` on board `88ED`.
- Use native `SBCC 0 0 0 0` for auto. It works on AC and battery; the
  `0x63 0 0 0` form requires AC.
- Verify mode changes with `GBCC`; do not trust the `SBCC` return package
  alone. `acpi_call` only treats strings beginning with `Error` as failures,
  but `SBCC` can return `0x34`, `0x35`, or `0x36` inside a successful call.
- Do not force-discharge to recover an overshoot. Put the machine on battery
  and let it reach the normal start threshold instead.

## ACPI/firmware semantics

`SBCC` on this board takes four integer arguments: `(Arg0, Arg1, 0, 0)`.

| Command | Meaning | Readback |
|---|---|---|
| `SBCC 0 0 0 0` | Auto; clears mode bits; works with or without AC | `GBCC 0` |
| `SBCC 0 1 0 0` | True inhibit; rejected with `0x36` while charging | `GBCC 1` |
| `SBCC 0 2 0 0` | Force-discharge; **never send** | `GBCC 2` |
| `SBCC 0x63 0 0 0` | Auto; requires `ADPP == 1` (AC present) | `GBCC 0` |
| `SBCC 0x63 1 0 0` | Force-discharge; **never send** | `GBCC 2` |
| `SBCC 0x63 2 0 0` | Hold; requires AC; writes `MBDC=0x0C` | `GBCC 3` |

`GBCC` logic in the DSDT:

- Requires `MBTS == 1`; otherwise reports `0xFF` (no battery).
- If `(MBDC & 0x18) == 0x18`, maps low bits:
  `0 -> 0`, `1 -> 1`, `2 -> 2`, `4 -> 3`.
- After a hold write, the EC sets bit 4, so `MBDC=0x1C` reports `3`.

Important implications:

- True inhibit (`SBCC 0 1`) cannot be entered while charging; do not rely on
  it on this board.
- `GBCC 1` is normal auto-charging on this machine, even though it can mean
  inhibit on other HP boards.
- `Mode::Auto` in the current code matches both `GBCC 0` and `GBCC 1`.
  Therefore auto verification alone cannot distinguish charging from a
  possible true-inhibit state. Prefer explicit logs and a hold readback when
  diagnosing.
- Force-discharge reads back as `GBCC 2`; it must never be accepted as
  success for inhibit or hold.

## Current code behavior

Repository: `/home/CheeseGuru/hp-charge-limit-rs`

- `src/firmware/acpi_call.rs`: Linux backend; calls `SBCC` with four native
  integer arguments; maps `Inhibit` and `Hold` to `0x63 2 0 0` on this board.
- `src/firmware/mod.rs`: `Mode`, `Reported`, `Reported::matches`.
  - Auto: accepts `GBCC 0` or `1`.
  - Inhibit: accepts `GBCC 3`, `1`, `4`, or `5`.
  - Hold: accepts `GBCC 3` only.
- `src/daemon/limiter.rs`: charge band and verification.
  - `stop=60`, `start=40`, poll interval 60 s.
  - `run-mode=hold`, `off-mode=hold`, `stop-mode=auto`.
  - Since commit `08181bb`, the daemon verifies immediately after a
    start/resume/bypass write, rechecks after 60 s (`VERIFY_SOON`), then
    verifies every 5 minutes (`VERIFY_EVERY`).
- `src/service/systemd.rs`: hardcodes `hp-charge-limit-rs.service`, but the
  NixOS unit is `hp-charge-limit.service`. This only makes the `status`
  command's service line wrong; `systemctl is-active hp-charge-limit.service`
  is authoritative.

## NixOS deployment

- Vendored source: `/etc/nixos/pkgs/hp-charge-limit-rs/`
- Package: `/etc/nixos/pkgs/hp-charge-limit-rs.nix`
- Service definition: `/etc/nixos/modules/system/power.nix`
- Unit: `hp-charge-limit.service`
- Current ExecStart flags:

```text
--stop 60 --start 40 --interval 60 --run-mode hold --off-mode hold --stop-mode auto --notify false
```

- `powerManagement.resumeCommands` runs
  `systemctl try-restart hp-charge-limit.service || true` after resume.
- The daemon stops with `auto` and restarts with `hold` across a failed or
  successful hibernate. This can briefly allow charging if the EC drops the
  hold state; the startup verification was added to close that gap.

Re-vendor after changing upstream:

```bash
rsync -a --delete --exclude '.git/' --exclude '.agents/' --exclude 'target/' \
  /home/CheeseGuru/hp-charge-limit-rs/ /etc/nixos/pkgs/hp-charge-limit-rs/
```

Build and activate:

```bash
nixos-rebuild test --flake /etc/nixos#Victussy
nixos-rebuild switch --flake /etc/nixos#Victussy
```

## Verification

```bash
systemctl is-active hp-charge-limit.service
journalctl -u hp-charge-limit.service -n 30 --no-pager
cat /sys/class/power_supply/BAT0/status \
    /sys/class/power_supply/BAT0/capacity \
    /sys/class/power_supply/ACAD/online
sudo /run/current-system/sw/bin/hp-charge-limit-rs status
```

Expected on this machine:

- At or below 40%: `start: cap=N% -> auto`; battery `Charging`; `GBCC 0`.
- At 60%: `limit: cap=N% -> hold`; `GBCC 3`; `Not charging`.
- On resume/restart: `start: cap=N% -> hold`, followed by a verify. A
  mismatch is logged as
  `verify: firmware reports auto (0), expected hold; setting hold`.
- The status command may print `service: hp-charge-limit-rs.service inactive`
  even when the real unit is active. Ignore that line.

## Hibernate findings

Hibernate is separate from the charge limiter. The limiter does not block it,
and it handles hold correctly across a successful hibernate.

### Successful hibernate/resume

- Entered hibernate **2026-11-09 18:13:58**, resumed **2026-11-10 10:09:03**
  with the same 4 GiB swap.
- Kernel: allocated 858,384 pages; needed 1,131,561 + 1024 pages; available
  2,888,770 pages.
- Resume succeeded. Kernel warned `ACPI: PM: Hardware changed while
  hibernated, success doubtful!` but the system resumed and removed the EFI
  `HibernateLocation` variable.
- After resume the daemon restarted with `start: cap=61% -> hold`.
- An earlier successful hibernate also occurred with the same 4 GiB swap:
  2026-11-08 13:13:07 -> 2026-11-09 06:43:59.

### Failure mode

- At **2026-11-09 15:31:02** hibernate failed with `Cannot allocate memory`
  and `PM: hibernation: Error -12 creating image`.
- Allocated 1,532,176 pages; needed 2,071,654 + 1024 pages; available
  1,948,732 pages. Shortage: 123,946 pages, about **484 MiB**.
- This was current memory pressure at image-allocation time, not a changed
  swap configuration.
- The image is compressed, so 4 GiB swap can work when memory usage is low.
  4 GiB is still marginal; 16 GiB swap is the reliable fix if failures recur.

### NVIDIA hibernate hook

- `nvidia-hibernate.service` failed repeatedly after the first failed
  hibernate with `nvidia-sleep.sh: line 45: echo: write error: Input/output
  error`.
- Later attempts failed at the dependency:
  `Dependency failed for System Hibernate`.
- The same hook succeeded before and after (notably on the successful
  18:13:58 hibernate). The failure appears stateful; reboot or clear the
  failed unit before retrying.
- At 18:12:27 `resumeCommands` logged a `try-restart` failure because the swap
  unit had a stop job queued. That did not prevent the eventual successful
  resume from restarting the daemon.

## Incident timeline

- 2026-11-09 15:31: Failed hibernate; daemon restarted. Old process set
  `auto`, new process set `hold` at 60%, but the EC dropped back to `auto`.
- 2026-11-09 15:36:19: Five-minute verification found `auto (0)` and reset
  hold; battery had reached 63%.
- 2026-11-09 15:51: Installed and tested the immediate/60-second verification
  fix. Forced `auto` at 15:51:32; daemon logged the mismatch and restored hold
  at 15:52:30.
- 2026-11-09 18:13:58: Hibernate entered; resumed 2026-11-10 10:09:03.
- 2026-11-10 10:09:04: Daemon restarted after resume and set hold at 61%.
- Current state at 10:19: battery `61%`, `Not charging`, AC online, firmware
  `hold (SBTC) (3)`, no failed units.

## Build and test

```bash
nix shell nixpkgs#cargo nixpkgs#rustc nixpkgs#clippy nixpkgs#gcc --command \
  cargo test --locked

nix shell nixpkgs#cargo nixpkgs#rustc nixpkgs#clippy nixpkgs#gcc --command \
  cargo clippy --all-targets --locked -- -D warnings
```

Current result: 12 tests pass; clippy clean; the Nix package has
`doCheck = true`.

Do not run `cargo fmt --all` or rely on `cargo fmt --check`: the current
rustfmt reformats pre-existing baseline files. Format only touched lines or
keep the existing style.

## Git and publication

- Upstream: `https://github.com/kralonur/hp-charge-limit-rs`
- Personal fork: `https://github.com/Ziggs25/hp-charge-limit-rs`
- Published machine branch: `victussy-88ed` (default branch of the fork)
- The published branch is based on v0.4.0 plus the local fixes above. The
  fork's `main` tracks upstream v0.4.2; do not assume they are mergeable
  without porting and re-testing. Upstream v0.4.2 has substantial backend and
  limiter changes.
- Local commits:
  - `dbb5a3d` `fix(limiter): verify mode immediately after start`
  - `c5ec488` `fix(firmware): avoid force-discharge SBCC commands on board 88ED`
  - `211fe2c` `fix(firmware): call SBCC with native 4 integer arguments for board 88ED`
  - `f09a28a` `fix(limiter): eliminate 30s reassertion loop ...`
- NixOS config: `/etc/nixos`, branch `chore/millennium-update`
  - `04f7f1c` `fix(power): verify charge mode after daemon start`
  - `3889daa` `fix(power): run charge limiter in hold mode`
  - Not merged into `main`; do not push to upstream without permission.

## Known issues / next steps

- `Mode::Auto` accepts `GBCC 1`, which is true inhibit on some HP boards.
  This is safe on 88ED (1 means auto charging) but should be fixed or
  documented for portability.
- `hp-charge-limit-rs status` still displays the wrong systemd unit name.
- Hibernate is not reliable under high memory pressure because the swap is
  only 4 GiB; enlarge swap to 16 GiB if it happens again.
- The NVIDIA hibernate hook can be left failed after an aborted hibernate.
  Reboot or `systemctl reset-failed nvidia-hibernate.service` before
  retrying.
- Do not attempt to lower a battery that overshot the stop threshold by
  forcing discharge. Let it run on battery below 40%, then recharge to 60%.
