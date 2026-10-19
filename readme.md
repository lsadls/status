# status

A single-line system monitor for Linux. Every second it prints one line with
CPU load/frequency/temperature, memory, battery, network and disk throughput,
filesystem space and the clock:

```
[cpu: 8% 1629MHz 47°C | mem: 7975MiB | bat: 99# | net: 85K/s↓ 21K/s↑ | disk: 127.4GiB | io: 532K/s↑ 0K/s↓ | date: 2026-10-19 14:31:25]
```

Each field is introduced by the label set in `[prefix]`; write a label empty
(`mem =`) to drop it, or clear all of them for the bare line:

```
[8% 1629MHz 47°C | 7975MiB | 99# | 85K/s↓ 21K/s↑ | 127.4GiB | 532K/s↑ 0K/s↓ | 2026-10-19 14:31:25]
```

It reads kernel pseudo-files (`/proc`, `/sys`) only — no daemon, no background
thread, no dependencies beyond `libc`/`chrono`. Anything that cannot be read on
the current machine degrades to an `n/a` token instead of aborting, so the same
binary works on a laptop with a battery and a desktop without one.

## Build and run

```sh
cargo build --release      # target/release/status
./target/release/status    # refresh loop
status --once              # one line, then exit (for panels and scripts)
```

Options:

| Flag | Meaning |
| --- | --- |
| `-c`, `--config <PATH>` | Read this config file instead of the default lookup |
| `-o`, `--once` | Print one line and exit |
| `-h`, `--help` | Print usage |

Exit codes: `0` success, `1` the explicitly requested config file could not be
read, `2` bad command line. With no config file at all the built-in defaults
are used (exit `0`).

## Configuration

The file is `status.ini` in the config directory, chosen by the first of:

1. `--config <PATH>`
2. `$STATUS_CONFIG`
3. `$XDG_CONFIG_HOME/status.ini` (default `~/.config/status.ini`)

A shipped example lives in [`status.ini`](status.ini). Every key is optional;
an unknown key, an empty value or an out-of-range number falls back to its
default *and prints a warning on stderr*, so a stale config never silently
changes what you see. Switches are words (`yes`/`no`, also `1`/`0`), and
choices take names: `figure=used`, `figure=available`.

Minimal example:

```ini
[cpu]
temp=yes              ; read the CPU temperature
zone=                 ; empty = auto-detect the thermal zone

[net]
show=yes              ; no = do not read any interface counters
interface=            ; empty = auto-detect an interface that is up
unit=0                ; 0 = KiB, 1 = MiB, 2 = GiB
suffix=K/s            ; empty prints bare numbers
auto=no               ; yes = step up to M/s as the rate grows

[disk]
device=nvme0n1        ; base device; empty = first physical disk
mount=/               ; mountpoint whose free space is shown
figure=available      ; available, used or total

[disk_size]
unit=2                ; capacity in GiB, as people quote disks
suffix=GiB

[disk_io]
show=yes              ; no = do not read /proc/diskstats
unit=0                ; throughput in KiB/s by default
```

Anything switched off is not computed and not displayed: the file behind it is
never opened, and the field is omitted rather than shown as `0`.

Full reference: [`docs/config.md`](docs/config.md).

## Documentation

- [`docs/index.md`](docs/index.md) — overview, quick start, output anatomy
- [`docs/config.md`](docs/config.md) — every key, its default and its effect
- [`docs/metrics.md`](docs/metrics.md) — how each number is computed and why
- [`docs/architecture.md`](docs/architecture.md) — module layout, sampling model, error policy
