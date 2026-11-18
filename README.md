# Battmon

A small, native battery monitor for GNOME: a tiny background sampler plus a
GTK4/libadwaita window. It reads `/sys/class/power_supply` directly — no UPower,
no D-Bus, no web stack.

- Live level, status, smoothed power draw and charge rate
- Time left while discharging, time to full while charging (with a taper model,
  so it does not promise "5 min" for half an hour at 99%)
- Health: full-charge capacity vs design capacity, with a daily trend
- Cycles from the firmware, or an honest estimate when the firmware reports 0
- Level history for the last 24 hours and 7 days, with charging periods shaded,
  suspend gaps left as gaps, and a hover readout
- Temperature or charge limit shown only when the firmware exposes them

## Install

Fedora 45 / GNOME 51. Build dependencies:

```sh
sudo dnf install gtk4-devel libadwaita-devel sqlite-devel pkgconf-pkg-config gcc
```

Then, as your user:

```sh
./scripts/install.sh
```

That builds a release binary, installs `battmon`, `battmond`, the desktop entry,
the icon and the `battmond.service` user unit, then enables and starts the
sampler. Nothing needs root and nothing is installed system-wide.

Uninstall:

```sh
systemctl --user disable --now battmond
rm -f ~/.local/bin/{battmon,battmond} \
      ~/.local/share/applications/design.beben.Battmon.desktop \
      ~/.local/share/icons/hicolor/scalable/apps/design.beben.Battmon.svg \
      ~/.config/systemd/user/battmond.service
rm -rf ~/.local/share/battmon     # this deletes the history
```

## Use

- **App**: open "Battery" from the Activities overview, or run `battmon`.
- **One reading, text**: `battmond --once`
- **Sampler**: `systemctl --user status battmond`,
  `journalctl --user -u battmond -f`
- **Database**: `~/.local/share/battmon/battmon.db` (WAL, readable with
  `sqlite3` while the daemon runs)

The window reads sysfs every 5 s while it is visible and re-queries history every
60 s; both timers stop when it is hidden. The daemon samples every 10 s, stores a
row every 60 s, and stores one immediately whenever the status or wall power
changes. Raw samples are kept for 30 days; daily health rows are kept forever.

History only exists from the moment the daemon first runs — day one is mostly
empty, and the window says so instead of inventing data.

## How it reads the battery

One module (`src/sysfs.rs`) touches `/sys`, and it accepts a root override
(`BATTMON_SYSFS_ROOT`) so everything can be tested against fixture trees.

Each field is resolved independently:

| Field | Primary | Fallback |
|---|---|---|
| Energy | `energy_now` (µWh) | `charge_now` (µAh) × `voltage_min_design` |
| Full | `energy_full` | `charge_full` × `voltage_min_design` |
| Design | `energy_full_design` | `charge_full_design` × `voltage_min_design` |
| Power | `power_now` (µW) | `current_now` × `voltage_min_design` |
| Level | energy / full | integer `capacity` file |

`voltage_min_design` is used for both Wh and W conversions so `energy / power`
stays consistent and the numbers match `upower -i`. The real reported voltage is
shown as the voltage. The development laptop only has the `charge_*` layout, no
`power_now`, no temperature and no charge limit, so that is the common path here;
the `energy_*` path is covered by fixtures.

Missing files degrade to `n/a`; nothing panics.

## Development

```sh
cargo test                        # formulas, sysfs fixtures, store, painters
cargo run --bin battmond -- --once
BATTMON_SYSFS_ROOT=tests/fixtures/charge_only/sys cargo run --bin battmond -- --once
```

Fixtures in `tests/fixtures` cover `charge_only`, `hp_energy` (energy layout),
`no_cycles`, `no_battery`, `ac_not_charging`, `capacity_only` and `garbage`.

To work on the charts without a week of real data:

```sh
cargo run --bin seed_db -- --db /tmp/battmon-seed.db --days 7 --reset
BATTMON_DB=/tmp/battmon-seed.db cargo run --bin battmon
```

`seed_db` refuses to run without an explicit `--db`, so it cannot touch the real
database. `BATTMON_DB` points the GUI at another database.

Crate versions are pinned to the system libraries: `gtk4` 0.11 with the `v4_24`
feature against GTK 4.24, and `libadwaita` 0.9 with `v1_10` against libadwaita
1.10. The daemon never links GTK (`cargo build --no-default-features`).

## Scope

No tray icon, no shell extension, no notifications, no writing to the battery
(the charge-limit threshold is displayed read-only if it exists), no Flatpak, no
per-app power attribution.

Known limits: HP firmware reports `cycle_count = 0` on some models (the UI then
shows an estimate labelled "≈"), and charge-limit controls depend on the kernel
and model.
