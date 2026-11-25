// SPDX-License-Identifier: GPL-2.0-only
mod config;
mod hardware;
mod os;
mod power;
use anyhow::{bail, ensure, Result};
use config::Config;
use hardware::Mode;
use os::*;
use std::{fs, path::Path};

const CONFIG: &str = "/etc/prismswitch/config.json";
const STATE: &str = "/run/prismswitch/eco.json";
const LOCK: &str = "/run/lock/prismswitch.lock";
const KEYD: &str = "/etc/keyd/prismswitch.conf";

fn usage() {
    println!("PrismSwitch {} — Two inputs. One screen.\nUsage: prismswitch <status|on|off|toggle|kick|profile NAME|restore|configure [--no-hotkeys]|hotkeys-install|validate|keyd-config>\nUse sudo for hardware, configuration and power actions.",env!("CARGO_PKG_VERSION"));
}
fn root_required() -> Result<()> {
    ensure!(root(), "run this command with sudo");
    Ok(())
}
fn config_write(c: &Config) -> Result<()> {
    c.validate()?;
    let mut bytes = serde_json::to_vec_pretty(c)?;
    bytes.push(b'\n');
    atomic(Path::new(CONFIG), &bytes, 0o644)
}
fn install_hotkeys(c: &Config) -> Result<()> {
    let path = Path::new(KEYD);
    ensure!(
        !dirs(path.parent().unwrap(), "")
            .iter()
            .any(|p| p.extension().is_some_and(|s| s == "conf") && p != path),
        "existing keyd configuration; use configure --no-hotkeys and merge keyd-config manually"
    );
    let text = c.keyd()?;
    let check = Path::new("/run/prismswitch/keyd-check.conf");
    atomic(check, text.as_bytes(), 0o600)?;
    let result = exec("keyd", &["check", check.to_str().unwrap()], 10);
    let _ = fs::remove_file(check);
    result?;
    let old = fs::read(path).ok();
    let was_active = active("systemctl", &["is-active", "--quiet", "keyd.service"]);
    atomic(path, text.as_bytes(), 0o644)?;
    let result = if was_active {
        exec("keyd", &["reload"], 15)
    } else {
        exec("systemctl", &["enable", "--now", "keyd.service"], 20)
    };
    if let Err(e) = result {
        if let Some(bytes) = old {
            atomic(path, &bytes, 0o644)?;
        } else {
            fs::remove_file(path)?;
        }
        if was_active {
            let _ = exec("keyd", &["reload"], 15);
        }
        return Err(e);
    }
    ensure!(
        active("systemctl", &["is-active", "--quiet", "keyd.service"]),
        "keyd not active"
    );
    println!("Physical keyboard bindings installed");
    Ok(())
}
fn configure(c: Config, hotkeys: bool) -> Result<()> {
    ensure!(
        !Path::new(STATE).exists(),
        "restore current profile before configuring"
    );
    let c = config::wizard(c)?;
    let path = Path::new(CONFIG);
    let old = fs::read(path).ok();
    config_write(&c)?;
    if hotkeys {
        if let Err(e) = install_hotkeys(&c) {
            if let Some(bytes) = old {
                atomic(path, &bytes, 0o644)?;
            } else {
                fs::remove_file(path)?;
            }
            return Err(e);
        }
    }
    println!("Configuration saved");
    Ok(())
}
fn run() -> Result<()> {
    signals();
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        [] | ["--help"] | ["-h"] => {
            usage();
            return Ok(());
        }
        ["--version"] | ["-V"] => {
            println!("prismswitch {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        ["validate"] => {
            config::load(Path::new(CONFIG))?;
            println!("Configuration valid");
            return Ok(());
        }
        ["keyd-config"] => {
            print!("{}", config::load(Path::new(CONFIG))?.keyd()?);
            return Ok(());
        }
        ["status"]
        | ["on"]
        | ["off"]
        | ["toggle"]
        | ["kick"]
        | ["restore"]
        | ["profile", _]
        | ["configure"]
        | ["configure", "--no-hotkeys"]
        | ["hotkeys-install"] => {}
        _ => {
            usage();
            bail!("invalid arguments")
        }
    }
    root_required()?;
    let _lock = lock(Path::new(LOCK))?;
    let sys = Path::new("/sys");
    let state_path = Path::new(STATE);
    if a == ["restore"] {
        return power::restore(sys, state_path);
    }
    let c = config::load(Path::new(CONFIG))?;
    match a.as_slice() {
        ["configure"] => return configure(c, true),
        ["configure", "--no-hotkeys"] => return configure(c, false),
        ["hotkeys-install"] => return install_hotkeys(&c),
        _ => {}
    }
    let dev = hardware::device(sys, &c)?;
    let current = hardware::native(&dev)?;
    let state = power::state(state_path)?;
    match a.as_slice() {
        ["status"] => {
            println!(
                "TDM={} profile={}{}",
                current.as_str(),
                state
                    .as_ref()
                    .map(|s| s.profile.as_str())
                    .unwrap_or("normal"),
                if state.as_ref().is_some_and(|s| !s.active) {
                    " (incomplete)"
                } else {
                    ""
                }
            );
            ensure!(
                !state.as_ref().is_some_and(|s| !s.active),
                "incomplete profile; run restore"
            );
            ensure!(
                current != Mode::Internal || hardware::edid_present(sys),
                "internal panel EDID unavailable"
            );
            return Ok(());
        }
        ["profile", name] => {
            ensure!(
                c.profiles.contains_key(*name),
                "unknown power profile: {name}"
            );
            let toggling_off = state
                .as_ref()
                .is_some_and(|s| s.active && s.profile == *name);
            power::restore(sys, state_path)?;
            if toggling_off {
                hardware::route(sys, &dev, Mode::Internal)?;
            } else {
                if current != Mode::External {
                    hardware::route(sys, &dev, Mode::External)?;
                }
                power::apply(sys, state_path, &c, name)?;
            }
        }
        ["kick"] => {
            power::restore(sys, state_path)?;
            hardware::route(sys, &dev, Mode::Internal)?;
            pause(3);
            hardware::route(sys, &dev, Mode::External)?;
            if let Some(s) = state.filter(|s| s.active) {
                if c.profiles.contains_key(&s.profile) {
                    power::apply(sys, state_path, &c, &s.profile)?;
                }
            }
        }
        ["on"] | ["off"] | ["toggle"] => {
            power::restore(sys, state_path)?;
            let target = match a[0] {
                "on" => Mode::External,
                "off" => Mode::Internal,
                _ => current.flip(),
            };
            if target != current || target == Mode::Internal {
                hardware::route(sys, &dev, target)?;
            } else {
                println!("External route already requested");
            }
        }
        _ => unreachable!(),
    }
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("PrismSwitch: {e:#}");
        std::process::exit(1);
    }
}
