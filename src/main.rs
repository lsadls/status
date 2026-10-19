//! `status` — a one-line system monitor for Linux.
//!
//! Prints CPU load/frequency/temperature, memory, battery, network and disk
//! throughput, filesystem space and the clock, refreshed continuously. See
//! `docs/` for the config reference and the metric definitions.

use std::{env, process::ExitCode};

mod bat;
mod config;
mod cpu;
mod date;
mod disk;
/// Captured kernel output used as regression fixtures (tests only).
#[cfg(test)]
mod fixtures;
mod mem;
mod metrics;
mod net;
mod prefix;
mod sysfs;
mod units;

use config::Config;
use metrics::{
    bat_from, cpu_from, date_from, disk_from, mem_from, net_from, Layout, Reading, Sampler,
    REFRESH_INTERVAL,
};

const USAGE: &str = "\
usage: status [OPTIONS]

Options:
  -c, --config <PATH>  read this config file instead of the default
  -o, --once           print a single line and exit (useful for scripting)
  -h, --help           show this help

Config file, first match wins:
  --config <PATH>, $STATUS_CONFIG, $XDG_CONFIG_HOME/status.ini,
  ~/.config/status.ini

All options are optional; with no config file the built-in defaults are used.
";

struct Args {
    config: Option<String>,
    once: bool,
}

/// Parse arguments. Returns `Err` with a message for the user (usage or a
/// mistyped flag) and `Ok(None)` for `--help`.
fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Option<Args>, String> {
    let mut out = Args {
        config: None,
        once: false,
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "-o" | "--once" => out.once = true,
            "-c" | "--config" => {
                let path = args.next().ok_or("--config needs a path")?;
                out.config = Some(path);
            }
            other => {
                // Support `--config=path` as well as `--config path`.
                if let Some(path) = other.strip_prefix("--config=") {
                    if path.is_empty() {
                        return Err("--config needs a path".to_string());
                    }
                    out.config = Some(path.to_string());
                } else {
                    return Err(format!("unknown argument {other:?}"));
                }
            }
        }
    }
    Ok(Some(out))
}

fn main() -> ExitCode {
    let args = match parse_args(env::args().skip(1)) {
        Ok(Some(args)) => args,
        Ok(None) => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("status: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };

    match args.config.as_deref().map(str::to_string) {
        Some(explicit) => match Config::load(Some(&explicit)) {
            Ok(loaded) => run(loaded.config, &loaded.warnings, args.once),
            Err(e) => {
                // An explicitly requested file must exist: that is a user error.
                eprintln!("status: cannot read {e}");
                ExitCode::FAILURE
            }
        },
        None => match Config::load(None) {
            Ok(loaded) => {
                eprintln!("status: config {}", loaded.path.display());
                run(loaded.config, &loaded.warnings, args.once)
            }
            Err(e) => {
                // No config: fall back to defaults instead of refusing to run,
                // which is what a freshly installed binary sees.
                eprintln!("status: {e}; using built-in defaults");
                run(Config::default(), &[], args.once)
            }
        },
    }
}

fn run(cfg: Config, warnings: &[String], once: bool) -> ExitCode {
    for warning in warnings {
        eprintln!("status: {warning}");
    }

    let mut local_warnings = Vec::new();
    let cpu = cpu_from(&cfg, &mut local_warnings);
    let mem = mem_from(&cfg, &mut local_warnings);
    let disk = disk_from(&cfg, &mut local_warnings);
    let bat = bat_from(&cfg, &mut local_warnings);
    let (net, nic) = net_from(&cfg, &mut local_warnings);
    let date = date_from(&cfg, &mut local_warnings);
    for warning in &local_warnings {
        eprintln!("status: {warning}");
    }

    let layout = Layout {
        nic,
        prefixes: prefix::Prefixes::from_config(&cfg),
    };

    let mut sampler = Sampler::new(cpu, mem, net, disk, bat, date, layout);
    for note in sampler.notes() {
        eprintln!("status: {note}");
    }

    if once {
        let reading = sampler.sample();
        print_line(&reading);
        return ExitCode::SUCCESS;
    }

    loop {
        // Slow block: filesystem space and interface detection change rarely,
        // so they are re-read once per REFRESH_INTERVAL instead of every line.
        if let Some(note) = sampler.rediscover() {
            eprintln!("status: {note}");
        }
        let deadline = std::time::Instant::now() + REFRESH_INTERVAL;
        loop {
            let reading = sampler.sample(); // sleeps SAMPLE_INTERVAL internally
            print_line(&reading);
            if std::time::Instant::now() >= deadline {
                break;
            }
        }
    }
}

/// Print one reading; a failed write (closed pipe) must not panic, so the
/// program simply exits on the next loop.
fn print_line(reading: &Reading) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    if writeln!(out, "{}", reading.format()).is_err() {
        std::process::exit(0);
    }
    let _ = out.flush();
}
