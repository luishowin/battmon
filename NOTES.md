# Battmon — machine notes and decisions

Recon collected on this laptop on 2026-11-18 (Fedora 45, GNOME Shell 51.1, Wayland,
kernel 7.3.3-300.fc45.x86_64). Everything below comes from the real machine, not
from the spec's assumptions.

## M0: what this machine actually exposes

`/sys/class/power_supply/`: `AC`, `BAT0`.

`BAT0` — Hewlett-Packard "Primary", Li-ion, serial `02228 2021/01/27`:

| attribute | value | notes |
|---|---|---|
| `status` | `Charging` | string parsed as-is |
| `charge_now` | 2 100 000 µAh | **charge layout, no `energy_*` at all** |
| `charge_full` | 2 683 000 µAh | |
| `charge_full_design` | 3 640 000 µAh | |
| `voltage_min_design` | 11 550 000 µV | conversion voltage |
| `voltage_now` | 12 502 000 µV | reported voltage only |
| `current_now` | 1 513 000 µA | **no `power_now`** |
| `capacity` | 78 | integer fallback for level |
| `capacity_level` | `Normal` | unused |
| `cycle_count` | **333** | non-zero → firmware value is trustworthy |
| `present` | 1 | |

Derived (UPower agrees to the last digit):
`energy 24.255 Wh`, `full 30.9886 Wh`, `design 42.042 Wh`, **`health 73.71%`**,
`power 17.475 W`, `level 78.27%`.

`AC`: `type=Mains`, `online=1`.

**Absent, therefore never shown:** `energy_now`/`energy_full`/`energy_full_design`,
`power_now`, `temp` (the `hwmon2/` node only has `curr1_input` and `in0_input`),
`charge_control_end_threshold`. The `alarm` and `extensions` nodes are unused.

## Decisions that follow from the recon

1. **Charge-first reader.** Spec §5 treats `energy_*` as primary; on this laptop
   that path never runs. `sysfs.rs` resolves each field independently:
   `energy_*` (µWh) when present, otherwise `charge_*` (µAh) ×
   `voltage_min_design` (µV). Both layouts are fixture-tested.

2. **`voltage_min_design` for both Wh and W.** Not `voltage_now`.
   `1.513 A × 12.502 V = 18.92 W`, but UPower reports `17.475 W`, which is
   `1.513 A × 11.55 V`. UPower also computes energy as `charge × 11.55 V`.
   Mixing the two voltages would inflate watts, `%/h` and the charge rate by
   ~8% relative to the stored energy. Using one nominal voltage keeps
   `energy / power` honest and matches UPower. `voltage_now` is still shown as
   the reported voltage.

3. **Cycles: firmware.** `cycle_count = 333 > 0`, so the UI shows `333`, not
   `≈ 333`. The estimate path (`cumulative_discharge_wh / design_wh`) is still
   implemented and fixture-tested (`no_cycles`, `cycle_count = 0`).

4. **Three stat cards, not four.** No `temp` and no charge limit on this model,
   so the "temperature or charge limit" card is omitted entirely (spec §9).
   Cards use `adw::WrapBox` so three of them sit in one row at 720 px and wrap
   to one column when narrow — no hardcoded 2×2 grid.

5. **App ID stays `design.beben.Battmon`** (user choice), even though the
   existing user units are `io.github.luishowin.*`.

## Toolchain / crate pins (verified, not guessed)

| system | version | crate | feature |
|---|---|---|---|
| GTK | 4.24.1 | `gtk4 0.11.5` (alias `gtk`) | `v4_22` — highest feature the crate has |
| libadwaita | 1.10.0 | `libadwaita 0.9.2` (alias `adw`) | `v1_10` — exact match |
| SQLite | 3.53.4 | `rusqlite` (system, **no** `bundled`) | `/usr/lib64/libsqlite3.so` present |
| rustc | 1.99.1 | edition 2024 | `gtk4-sys` needs ≥ 1.92 |

APIs confirmed in the crate sources under `~/.cargo/registry/src`:

- `adw::ToggleGroup` (`set_active`, `connect_active_notify`) for 24 h / 7 d.
- `adw::WrapBox`, `adw::Breakpoint`, `adw::Banner`, `adw::StatusPage`,
  `adw::Clamp`, `adw::ToolbarView` all present.
- `adw::StyleManager::accent_color_rgba()` for the chart accent.
- `gtk::DrawingArea`'s draw callback is `DrawingAreaExtManual::set_draw_func`
  (a manual trait, not an inherent method).
- `gtk::EventControllerMotion::connect_motion` for the crosshair.

## Data / paths

- DB: `$XDG_DATA_HOME/battmon/battmon.db`, i.e. `~/.local/share/battmon/battmon.db`
  (`XDG_DATA_HOME` is unset here).
- Sysfs root override: `BATTMON_SYSFS_ROOT`, pointing at a sysfs root
  (`<root>/class/power_supply/...`), used by every test.
- Sampling: 10 s tick, 60 s persist, immediate persist on status/AC change.

## Things to watch

- `current_now` is positive while charging here; on discharge it is normally
  negative. Power is stored as a magnitude and direction comes from `status`.
- The battery is at 73.7% health, so `charge_full` drift is real and the daily
  health trend is meaningful rather than noise.
- Taper is already visible: 17.5 W at 78%, 10.9 W at 89%. The daemon was
  started early specifically to capture this curve for §6.4.
- UPower's "time to full" (23.1 min at 78%) is not linear
  (`4.04 Wh / 17.5 W ≈ 14 min`), so exact agreement there is not expected; the
  taper model is the point.
- Gaps from suspend are kept as gaps in the charts, per spec.

## M6: packaging, measurements, and two things worth remembering

Installed with `scripts/install.sh`: binaries in `~/.local/bin`, desktop entry in
`~/.local/share/applications`, icon in `~/.local/share/icons/hicolor/scalable/apps`,
unit in `~/.config/systemd/user`, service enabled and running.
`gtk-launch design.beben.Battmon` opens the window.

| what | measured on this laptop |
|---|---|
| `battmon` binary | 714 KB (stripped, LTO, `opt-level = "z"`) |
| `battmond` binary | 682 KB, no GTK in `ldd` |
| `battmond` idle | **0.01 % CPU** (1 tick per 120 s), 4.8 MB RSS, one row per 60 s |
| `battmon` idle, visible | 0.03 % CPU, **PSS 30 MB** (VmRSS 99 MB, 78 MB of it shared toolkit/GPU pages) |
| stock GTK4 apps here | 110–138 MB VmRSS (calculator 138, text editor 118, clocks 111) |
| one full sysfs read | ~0.15 ms; 10 `battmond --once` runs take 19 ms including process startup |

Two traps found while measuring:

1. **Seconds vs milliseconds in the sampler sleep.** `next_tick - now` is in
   seconds; passing it to `from_millis` made the loop wake at ~100 Hz and cost
   ~0.2 % CPU. `sleep_between()` now owns the conversion and has a regression
   test (`sleep_between(1010, 1000) == 1 s`).
2. **`AdwClamp` eases the page narrower than `maximum-size`.** Its threshold
   must be *above* the maximum for a hard cap; with the default 400 px a 720 px
   window got a ~610 px page, because `clamp_layout_allocate()` interpolates
   from the threshold over a 960 px ramp. We use `maximum_size(720)` +
   `tightening_threshold(1000)`, so the page is 720 px and fills anything
   narrower.

Also: `Widget::color()` (theme foreground) and
`StyleManager::accent_color_rgba()` drive every chart colour, so dark/light and
the accent follow the system without any hardcoded palette.

