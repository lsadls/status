# Configuration reference

The config file is a small INI subset:

- `[section]` headers — case-insensitive
- `key = value` — key case-insensitive, surrounding whitespace trimmed
- `#` and `;` start a comment line (`#`/`;` **only at line start**, not trailing)
- a bare `key` (or `key =`) means "key present, empty value"

Unknown sections and keys are ignored, so a config written for an older version
keeps working. Every key is optional.

The file shipped with the source, [`../status.ini`](../status.ini), is the
readable version of this page: every key appears there once, next to the line it
affects. This document is the precise reference — types, defaults, edge cases.

## Location

The file is `status.ini` in the config directory:

1. `--config <PATH>`
2. `$STATUS_CONFIG`
3. `$XDG_CONFIG_HOME/status.ini` (default `~/.config/status.ini`)

There is no per-directory or subdirectory variant: one program, one file path.
If `--config` names a file that cannot be read, the program exits with status
`1`; if the default location has no file, built-in defaults are used and the
program still runs (exit `0`). Which file was loaded is reported on stderr.

## Value syntax

| Key type | Accepted | Notes |
| --- | --- | --- |
| Switch | `yes`/`no` (also `on`/`off`, `true`/`false`, `1`/`0`) | Case-insensitive; anything else warns and keeps the default |
| Choice | the name (`used`, `available`, `total`) | Names only — a number would depend on an ordering the file never states |
| Amount `unit` | `0` = KiB, `1` = MiB, `2` = GiB | Higher values clamp to GiB with a warning |
| Path/name/text | as written | Empty value = "auto"/default |
| `suffix` | text | Omitted uses the metric's default label; written empty prints no label |

## Validation rules

| Input | Behaviour |
| --- | --- |
| Key absent | Default value |
| Key present, empty value | Default value (this is how `zone`/`interface`/`fmt` mean "auto") |
| `suffix` present, empty value | No label at all — not the default one |
| Switch word that is neither yes nor no | Default value + warning naming the key |
| Number not a number, or > 255 | Default value + warning |
| Number outside a key's documented set (e.g. `figure = 7`) | Falls back to the key's default |
| Unknown section or key | Ignored silently (forward compatibility) |

Warnings are printed to stderr, and a missing metric never stops the program.

## `[cpu]`

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `temp` | switch | `no` | Read the CPU temperature at all. When off, no thermal zone is probed |
| `zone` | path | empty | Thermal zone directory (`/sys/class/thermal/thermal_zone0`) or the `temp` file itself. Empty = auto-detect. Ignored when `temp = no` |
| `mhz` | switch | `yes` | Show the frequency in MHz. When off, the `cpu MHz` field of `/proc/cpuinfo` is never read |
| `per_core` | switch | `no` | Report usage per core instead of across the machine. With `yes` the core count is read from `/proc/cpuinfo` and the percentage can exceed 100 (`1600%` on 16 cores, as `htop` shows); with `no` the number is the whole-machine share, the same as `top`'s `%Cpu(s)` |

Usage is always shown and has no switch: it is the reason the program exists.

Auto-detection scans `/sys/class/thermal/thermal_zone*`, reads each `type`, and
prefers, in order: `x86_pkg_temp`, `coretemp`, `k10temp`, `acpitz`,
`cpu-thermal`, `soc_thermal`. A plain `acpitz` zone is a chipset reading, not a
core reading — expect it to lag and to under-report under sustained load, which
is why it ranks low. If nothing matches, the temperature is reported as
unavailable on stderr and the field shows `n/a °C`.

Usage, frequency and temperature form a single field, separated by spaces and
never by `|`, because all three describe the same device:

```
12% 2400MHz 45°C
```

Disabling either part removes it without leaving a gap: with `mhz = no` the
field reads `12% 45°C`, and with both off it is just `12%`.

```ini
[cpu]
temp=yes
zone=/sys/class/thermal/thermal_zone0
mhz=yes
per_core=no
```

Usage has no switch of its own — it is always shown — but it has two
scales, and they answer different questions:

```
6%          # whole machine: one core of sixteen busy (default)
100%        # per core: that same core, measured against itself
```

`per_core = yes` multiplies by the logical core count, so a fully loaded
sixteen-core box reads `1600%`. When the switch is off the core count is never
read; when the CPU has no `/proc/cpuinfo` to count, the machine figure is used.

## `[mem]`

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `figure` | `used`/`available`/`total` | `used` | Which number to print |
| `unit` | `0`/`1`/`2` | `1` | `0` = KiB, `1` = MiB, `2` = GiB |
| `suffix` | text | `MiB` | Printed after the number. A written-but-empty value prints the bare number |

"Used" is `MemTotal - MemAvailable`, not `MemFree`: the kernel's `MemFree`
excludes reclaimable page cache, which makes a healthy machine look 95 % full.

Memory defaults to MiB, matching `free -m`. The space figure keeps one decimal
place when `unit = 2` (`7.8GiB`), as described under [`[disk_size]`](#disk_size).

```ini
[mem]
figure=used
unit=1
suffix=MiB
```

## `[bat]`

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `show` | switch | `no` | Show the battery field |
| `path` | path | empty | Battery directory (`/sys/class/power_supply/BAT0`). Empty = first `BAT*` entry |

With `show = yes` but no battery present, a warning is printed and the field is
omitted — the rest of the line is unaffected.

```ini
[bat]
show=yes
path=
```

## `[net]`

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `show` | switch | `yes` | Show throughput. When off, interface discovery and every counter read are skipped |
| `interface` | name | empty | Interface to watch. Empty = auto-detect. Ignored when `show = no` |
| `unit` | `0`/`1`/`2` | `0` | Base unit: `0` = KiB, `1` = MiB, `2` = GiB |
| `suffix` | text | `K/s` | Printed after the number, e.g. `suffix=KB` renders `2KB/s↓`. A suffix starting with `K`/`M`/`G` also selects that unit. Written but empty = no label and no `/s` |
| `auto` | switch | `no` | Step the unit up as the rate grows: with `yes`, `2048K/s` becomes `2M/s`. Off by default so the unit stays what you asked for |

An explicit `interface` must exist under `/sys/class/net`; otherwise the field
shows `n/a net` and the reason goes to stderr. Auto-detection sorts the
interface names, skips `lo`, skips interfaces whose `carrier` cannot be read (a
down interface), and prefers physical-looking names (`en*`, `wl*`, `ww*`,
`eth*`, `wlan*`, `usb*`) over virtual ones (`virbr*`, `veth*`, `docker*`,
`br-*`, `tun*`, `tap*`, `vnet*`, `vmnet*`). If nothing qualifies it falls back
to `lo`.

Rates are per second, measured over a fixed one-second window
(see [metrics.md](metrics.md)).

The `/s` is part of the unit, so `suffix=MB/s` renders `2MB/s↓` rather than
`2MB/s/s↓`; a suffix without `/s` gets it appended. A suffix left empty prints
the bare number (`275↓`) — no unit letter, no `/s`.

```ini
[net]
show=yes
interface=
unit=0
suffix=K/s
auto=no
```

## `[disk]`

The device and the filesystem it holds. Throughput has its own section,
[`[disk_io]`](#disk_io), because it has its own format and its own switch.

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `device` | device | empty | Whole block device name as printed by `lsblk -dn -o NAME`. Empty = first physical disk |
| `mount` | path | `/` | Mountpoint whose filesystem size is reported |
| `figure` | `available`/`used`/`total` | `available` | Which capacity number to print |

`device` must be the **base device**, not a partition: `nvme0n1`, not
`nvme0n1p1`; `sda`, not `sda1`. Partition names also appear in
`/proc/diskstats`, but their counters are already included in the parent
device, so summing them double-counts on some stacks and undercounts on
others. If the name is not present, the field shows `n/a io` and stderr names
the missing device — the old code reported `0` here, which was indistinguishable
from a genuinely idle disk.

An empty `device` auto-detects: `/sys/block` is read, entries without a `device`
symlink (virtual devices such as `zram0`, `loop0`, `dm-0`, whose counters stay
at zero) are skipped, and the first name in sorted order is used. `/sys/block`
lists whole devices only, so a partition cannot be picked by accident. The
built-in default used to be `sda`, which silently reported `n/a io` on every
NVMe machine.

```ini
[disk]
device=nvme0n1
mount=/
figure=available
```

`figure = available` counts `f_bavail`, the space a normal user can actually
write, which excludes the root-reserved blocks — on ext4 that is typically 5 %
of the filesystem, so "available" is deliberately smaller than
"total − used".

## `[disk_size]`

Capacity format — the filesystem figure described above.

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `unit` | `0`/`1`/`2` | `2` | `0` = KiB, `1` = MiB, `2` = GiB. The default is GiB because a disk is quoted in gigabytes, not megabytes |
| `suffix` | text | `GiB` | Printed after the number. `suffix=GB` renders `127.5GB`; written but empty prints `127.4` |

Capacity does not accept `auto`: a filesystem size does not change between
lines, so a unit flipping from `1023.9MiB` to `1.0GiB` would only make the
column jitter.

The number always keeps one decimal (`127.5GiB`) so that a small change stays
visible; rates are whole numbers instead.

```ini
[disk_size]
unit=2
suffix=GiB
```

## `[disk_io]`

Throughput format — the two rates from `/proc/diskstats`.

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `show` | switch | `yes` | Show throughput. When off, `/proc/diskstats` is never read; the capacity figure is unaffected |
| `unit` | `0`/`1`/`2` | `0` | `0` = KiB, `1` = MiB, `2` = GiB. The default is KiB/s: disk activity is normally reported in kilobytes |
| `suffix` | text | `K/s` | Printed after the number. Written but empty = no label and no `/s` |
| `auto` | switch | `no` | Step up as the rate grows (`2048K/s` → `2M/s`) |

The switch sits here, with the format it controls, rather than in `[disk]`,
which describes *what* to measure. Exactly one section decides whether
throughput is read.

```ini
[disk_io]
show=yes
unit=0
suffix=K/s
auto=no
```

## Suffix and unit rules

`unit`, `suffix` and `auto` appear in `[mem]`, `[net]`, `[disk_size]` and
`[disk_io]` with the same meaning:

- `unit` picks the divisor.
- `suffix` is the label. Omitting the key uses that metric's default
  (`MiB`, `GiB`, `K/s`); writing the key empty removes the label entirely, so
  the number stands alone and a rate loses its `/s` too. A suffix starting
  with `K`, `M` or `G` also selects that unit, because the printed label and
  the divisor must never disagree — `suffix=MB` on a `GiB` setting would
  otherwise show a MiB number labelled `MB`.
- A suffix that is not a unit word (e.g. `packets`) is printed verbatim and
  never rewritten by `auto`.
- `auto` steps the unit *up* only, starting from the configured one, and never
  below it.

## `[prefix]`

Text printed in front of each field, so a number says what it measures without
a legend:

```
[cpu: 11% 1530MHz 49°C | mem: 7961MiB | disk: 127.4GiB | io: 72K/s↑ 0K/s↓]
```

| Key | Default | Labels |
| --- | --- | --- |
| `cpu` | `cpu:` | CPU usage, frequency, temperature |
| `mem` | `mem:` | Memory figure |
| `bat` | `bat:` | Battery field (only present when `[bat] show = yes`) |
| `net` | `net:` | Network rates |
| `disk_size` | `disk:` | Filesystem capacity |
| `disk_io` | `io:` | Disk throughput |
| `date` | `date:` | Clock |

Two fields come from `[disk]`, so they get two labels: `disk:` for capacity and
`io:` for throughput, which is what the line itself distinguishes.

A key written empty removes that label (the number stands alone); a key left
out keeps its default. Clearing all seven gives exactly the unlabelled line:

```ini
[prefix]
cpu=
mem=
bat=
net=
disk_size=
disk_io=
date=
```

## `[date]`

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `fmt` | strftime | `%Y-%m-%d %H:%M:%S` | Clock format; empty = the default. A pattern chrono cannot render (e.g. `%Q`, trailing `%`) is rejected with a warning and the default is used |

```ini
[date]
fmt=%H:%M:%S
```

## Complete example

[`../status.ini`](../status.ini) documents every key inline and is a working
starting point:

```ini
[prefix]
cpu=cpu:
mem=mem:
bat=bat:
net=net:
disk_size=disk:
disk_io=io:
date=date:

[cpu]
temp=yes
zone=
mhz=yes
per_core=no

[mem]
figure=used
unit=1
suffix=MiB

[bat]
show=yes
path=

[net]
show=yes
interface=
unit=0
suffix=K/s
auto=no

[disk]
device=nvme0n1
mount=/
figure=available

[disk_size]
unit=2
suffix=GiB

[disk_io]
show=yes
unit=0
suffix=K/s
auto=no

[date]
fmt=%m-%d %H:%M:%S
```
