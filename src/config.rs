// SPDX-License-Identifier: GPL-2.0-only
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, IsTerminal, Write},
    path::Path,
};

const MODS: [&str; 4] = ["control", "alt", "shift", "meta"];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub cpu_percent: u8,
    pub disable_boost: bool,
    pub pause_services: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub allow_experimental: bool,
    pub blank_internal: bool,
    pub user: String,
    pub shortcuts: BTreeMap<String, String>,
    pub profiles: BTreeMap<String, Profile>,
}
impl Default for Config {
    fn default() -> Self {
        serde_json::from_str(include_str!("../config.example.json"))
            .expect("valid embedded defaults")
    }
}
pub fn load(path: &Path) -> Result<Config> {
    let c = match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).context("invalid configuration JSON")?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Config::default(),
        Err(e) => return Err(e.into()),
    };
    c.validate()?;
    Ok(c)
}
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name.as_bytes()[0].is_ascii_lowercase()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        && ![
            "toggle",
            "on",
            "off",
            "status",
            "configure",
            "kick",
            "restore",
            "profile",
            "validate",
            "keyd-config",
        ]
        .contains(&name)
}
pub fn service_name(s: &str) -> bool {
    s.ends_with(".service")
        && s.len() > 8
        && s.len() < 256
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.@-".contains(&b))
        && ![
            "pipewire",
            "wireplumber",
            "dbus",
            "plasma",
            "keyd",
            "systemd",
            "networkmanager",
        ]
        .iter()
        .any(|prefix| s.to_ascii_lowercase().starts_with(prefix))
}
pub fn chord(value: &str) -> Result<(String, String)> {
    let lower = value.to_ascii_lowercase();
    let mut parts: Vec<&str> = lower.split('+').collect();
    let key = parts.pop().context("missing shortcut key")?;
    let valid_key = (key.len() == 1 && key.as_bytes()[0].is_ascii_alphanumeric())
        || key
            .strip_prefix('f')
            .and_then(|s| s.parse::<u8>().ok())
            .is_some_and(|n| (1..=12).contains(&n) && key == format!("f{n}"));
    ensure!(valid_key, "shortcut key must be a letter, digit or F1–F12");
    let mut seen = BTreeSet::new();
    for part in parts {
        let m = match part {
            "ctrl" => "control",
            "super" | "win" => "meta",
            other => other,
        };
        ensure!(
            MODS.contains(&m) && seen.insert(m),
            "invalid or duplicate shortcut modifier"
        );
    }
    ensure!(
        seen.len() >= 2,
        "use at least two distinct shortcut modifiers"
    );
    Ok((
        MODS.into_iter()
            .filter(|m| seen.contains(m))
            .collect::<Vec<_>>()
            .join("+"),
        key.into(),
    ))
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.profiles.is_empty() && self.shortcuts.contains_key("toggle"),
            "normal toggle and at least one profile required"
        );
        let user = self.user.as_bytes();
        ensure!(
            user.is_empty()
                || ((user[0].is_ascii_alphabetic() || user[0] == b'_')
                    && user
                        .iter()
                        .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(b))
                    && self.user != "root"),
            "invalid non-root username"
        );
        for (name, p) in &self.profiles {
            ensure!(valid_name(name), "invalid profile name: {name}");
            ensure!(p.cpu_percent <= 100, "CPU percentage must be 0..100");
            let mut services = BTreeSet::new();
            for s in &p.pause_services {
                ensure!(
                    !self.user.is_empty() && service_name(s) && services.insert(s),
                    "invalid, protected or duplicate user service: {s}"
                );
            }
        }
        let mut used = BTreeSet::new();
        for (name, key) in &self.shortcuts {
            ensure!(
                name == "toggle" || self.profiles.contains_key(name),
                "unknown shortcut action: {name}"
            );
            ensure!(used.insert(chord(key)?), "duplicate shortcut: {key}");
        }
        Ok(())
    }
    pub fn keyd(&self) -> Result<String> {
        self.validate()?;
        let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (name, value) in &self.shortcuts {
            let (layer, key) = chord(value)?;
            let action = if name == "toggle" {
                "toggle".into()
            } else {
                format!("profile {name}")
            };
            groups.entry(layer).or_default().push(format!(
                "{key} = command(/usr/local/bin/prismswitch {action})"
            ));
        }
        let mut out =
            String::from("# PrismSwitch: physical shortcuts; other keys pass through.\n[ids]\n*\n");
        for (layer, bindings) in groups {
            out.push_str(&format!("\n[{layer}]\n{}\n", bindings.join("\n")));
        }
        Ok(out)
    }
}
fn ask(prompt: &str) -> Result<String> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut line = String::new();
    ensure!(
        io::stdin().read_line(&mut line)? > 0,
        "configuration cancelled"
    );
    Ok(line.trim().to_owned())
}
fn yesno(prompt: &str, current: bool) -> Result<bool> {
    match ask(&format!(
        "{prompt} [{}] (y/n): ",
        if current { "y" } else { "n" }
    ))?
    .as_str()
    {
        "" => Ok(current),
        "y" | "Y" => Ok(true),
        "n" | "N" => Ok(false),
        _ => bail!("enter y or n"),
    }
}
pub fn wizard(mut c: Config) -> Result<Config> {
    ensure!(io::stdin().is_terminal(), "interactive terminal required");
    println!("PrismSwitch | Two inputs. One screen.");
    loop {
        let name = ask("Add a named profile (Enter to continue): ")?;
        if name.is_empty() {
            break;
        }
        ensure!(
            valid_name(&name) && !c.profiles.contains_key(&name),
            "invalid or duplicate profile name"
        );
        c.profiles.insert(
            name,
            Profile {
                cpu_percent: 50,
                disable_boost: true,
                pause_services: vec![],
            },
        );
    }
    let actions = std::iter::once("toggle".to_string())
        .chain(c.profiles.keys().cloned())
        .collect::<Vec<_>>();
    for name in actions {
        let old = c.shortcuts.get(&name).map(String::as_str).unwrap_or("none");
        let v = ask(&format!("{name} shortcut [{old}] (Enter keep, - remove): "))?;
        if v == "-" {
            ensure!(name != "toggle", "normal shortcut required");
            c.shortcuts.remove(&name);
        } else if !v.is_empty() {
            c.shortcuts.insert(name, v);
        }
    }
    let v = ask(&format!(
        "Account for optional user-service pausing [{}] (Enter keep): ",
        c.user
    ))?;
    if !v.is_empty() {
        c.user = v;
    }
    for (name, p) in &mut c.profiles {
        let v = ask(&format!(
            "{name}: CPU range 0=min,100=original [{}]: ",
            p.cpu_percent
        ))?;
        if !v.is_empty() {
            p.cpu_percent = v.parse()?;
        }
        p.disable_boost = yesno(&format!("{name}: disable turbo?"), p.disable_boost)?;
        if !c.user.is_empty() {
            let v = ask(&format!(
                "{name}: services to pause [{}] (comma-separated, - clear): ",
                p.pause_services.join(",")
            ))?;
            if v == "-" {
                p.pause_services.clear();
            } else if !v.is_empty() {
                p.pause_services = v.split(',').map(|s| s.trim().into()).collect();
            }
        }
    }
    c.blank_internal = yesno("Blank internal output in power profiles?", c.blank_internal)?;
    c.validate()?;
    Ok(c)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults() {
        let c = Config::default();
        c.validate().unwrap();
        let text = c.keyd().unwrap();
        assert!(text.contains("m = command(/usr/local/bin/prismswitch toggle)"));
        assert!(text.contains("e = command(/usr/local/bin/prismswitch profile eco)"));
    }
    #[test]
    fn duplicate() {
        let mut c = Config::default();
        c.shortcuts.insert("eco".into(), "Alt+Control+M".into());
        assert!(c.validate().is_err());
    }
    #[test]
    fn unsafe_chords() {
        for v in [
            "Ctrl+Alt+M;rm",
            "Ctrl+Alt+$(x)",
            "Ctrl+M",
            "Ctrl+Ctrl+M",
            "Ctrl+Alt+F13",
            "Ctrl+Alt+F01",
        ] {
            assert!(chord(v).is_err(), "{v}");
        }
    }
    #[test]
    fn custom() {
        let mut c = Config::default();
        c.profiles.insert(
            "quiet".into(),
            Profile {
                cpu_percent: 35,
                disable_boost: true,
                pause_services: vec![],
            },
        );
        c.shortcuts.insert("quiet".into(), "Super+Shift+F6".into());
        assert!(c
            .keyd()
            .unwrap()
            .contains("[shift+meta]\nf6 = command(/usr/local/bin/prismswitch profile quiet)"));
    }
    #[test]
    fn unsafe_profile() {
        let mut c = Config::default();
        c.profiles.insert("a;rm".into(), c.profiles["eco"].clone());
        assert!(c.validate().is_err());
    }
    #[test]
    fn services() {
        for v in [
            "--x.service",
            "pipewire.service",
            "wireplumber.service",
            "syncthing;rm.service",
            "dbus.service",
        ] {
            assert!(!service_name(v), "{v}");
        }
        assert!(service_name("syncthing.service"));
    }
    #[test]
    fn explicit_user() {
        let mut c = Config::default();
        c.profiles
            .get_mut("eco")
            .unwrap()
            .pause_services
            .push("syncthing.service".into());
        assert!(c.validate().is_err());
        c.user = "demo".into();
        assert!(c.validate().is_ok());
    }
    #[test]
    fn strict_json() {
        assert!(serde_json::from_str::<Config>("{}").is_err());
    }
}
