# status — overview

`status` prints one line of live system information per second, reading only
kernel pseudo-files. This page explains what the output means and how the
program behaves; the other pages go into detail.

| Page | Contents |
| --- | --- |
| [config.md](config.md) | Every config key, default, and effect |
| [metrics.md](metrics.md) | How each number is derived from `/proc` and `/sys` |
| [architecture.md](architecture.md) | Modules, sampling model, error policy, testing |

## Quick start

```sh
cargo build --release
./target/release/status --once     # one line for a script or panel
./target/release/status            # continuous refresh, Ctrl-C to stop
```

The program needs no arguments: with no config file it uses built-in defaults
and prints what it can read. Diagnostics (which config file was loaded, which
interface was auto-detected, which keys were rejected) go to **stderr**, so
stdout stays a clean machine-readable line.

## Output anatomy

A full line, one field per metric, separated by ` | `, each field introduced by
the label configured in `[prefix]`:

```
[cpu: 8% 1555MHz 47°C | mem: 7969MiB | bat: 99# | net: 35K/s↓ 10K/s↑ | disk: 127.4GiB | io: 76K/s↑ 0K/s↓ | date: 2026-10-19 14:31:32]
```

Labels can be renamed or removed one by one (`[prefix] mem =` drops the label,
leaving `7969MiB`). Labels are **on by default**; clearing all seven gives the
bare line, shown below only because the arrow diagram reads better without
them:

```
[8% 1555MHz 47°C | 7969MiB | 99# | 35K/s↓ 10K/s↑ | 127.4GiB | 76K/s↑ 0K/s↓ | 2026-10-19 14:31:32]
```

Reading the unlabelled form left to right:

```
[8% 1555MHz 47°C | 7969MiB | 99# | 35K/s↓ 10K/s↑ | 127.4GiB | 76K/s↑ 0K/s↓ | 2026-10-19 14:31:32]
 │   │     │      │        │     │     │       │          │        │      └─ clock (strftime)
 │   │     │      │        │     │     │       │          │        └─ disk write rate
 │   │     │      │        │     │     │       │          └─ disk read rate
 │   │     │      │        │     │     │       └─ filesystem figure ([disk] figure, [disk_size])
 │   │     │      │        │     │     └─ network transmit
 │   │     │      │        │     └─ network receive
 │   │     │      │        └─ battery: state symbol, power, remaining time
 │   │     │      └─ memory figure ([mem] figure, [mem] unit)
 │   │     └─ CPU temperature, °C (omitted when [cpu] temp = no)
 │   └─ average CPU frequency, MHz (omitted when [cpu] mhz = no or unreadable)
 └─ CPU busy percentage since the previous line
```

Usage, frequency and temperature are one group: they are three readings of the
same device and are separated by spaces, not by ` | `. Each of the other
fields is its own group.

Ordering is fixed; optional fields are **omitted**, never rendered blank. A
field that is switched off in the config is also not read: the syscall or file
behind it is skipped, so the omission costs nothing.

### Field reference

| Field | Example | Unit | Notes |
| --- | --- | --- | --- |
| CPU busy | `7%` | % | Whole-machine share for the last sampling interval (as `top`'s `%Cpu(s)`). `[cpu] per_core = yes` switches to the `htop` scale, where a core is 100 % and a 16-core box tops out at `1600%` |
| CPU frequency | `1478MHz` | MHz | Mean of the per-core `cpu MHz` values in `/proc/cpuinfo` |
| CPU temperature | `47°C` | °C | Omitted if `[cpu] temp = no`; `n/a °C` if the sensor vanished |
| Memory | `7969MiB` | KiB/MiB/GiB | Figure chosen by `[mem] figure` (used/available/total); MiB by default |
| Battery | `99#` | — | `+` charging, `-` discharging, `#` idle/full, `?` unknown; `12.0W` power and `2:30` remaining time appear only when charge is actually moving. A non-discharging battery at `100` prints just `100` |
| Network | `35K/s↓ 10K/s↑` | KiB/s … | Receive then transmit; `[net] unit`/`suffix`/`auto`, KiB/s by default |
| Filesystem | `127.4GiB` | KiB/MiB/GiB | One decimal place; `[disk_size] unit`, GiB by default |
| Disk I/O | `76K/s↑ 0K/s↓` | KiB/s … | Write then read; `[disk_io] unit`/`suffix`/`auto`, KiB/s by default |
| Clock | `2024-01-02 03:04:05` | — | `[date] fmt`, strftime syntax |

Note the arrow direction: the network field shows **receive (`↓`) first**,
the disk field shows **write (`↑`) first**. That matches what each one is
usually asked about — "am I downloading?" and "is the disk writing?".

## Failure behaviour

Nothing in the read path can abort the line. A metric that cannot be read is
replaced by a short token:

| Situation | Output |
| --- | --- |
| Sensor file missing / unreadable | `n/a` token for that field |
| Battery disabled, or no battery present | field omitted |
| Temperature disabled | field omitted |
| CPU counters did not advance | `0%` |
| Device or interface from the config does not exist | `n/a io` / `n/a net`, with the reason on stderr |

The program keeps running in all of these cases; only a closed stdout ends it
(so `status | head -1` exits cleanly instead of panicking).
