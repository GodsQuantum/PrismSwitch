// SPDX-License-Identifier: GPL-2.0-only
use crate::{config::Config, hardware, os::*};
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};
pub const UNIT: &str = "prismswitch-inhibit.service";
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub profile: String,
    pub active: bool,
    pub user: String,
    pub services: Vec<String>,
    pub settings: Vec<(PathBuf, String)>,
    pub dpms: Option<String>,
}
pub fn state(path: &Path) -> Result<Option<State>> {
    match fs::read(path) {
        Ok(b) => Ok(Some(
            serde_json::from_slice(&b).context("invalid recovery state")?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
fn save(path: &Path, s: &State) -> Result<()> {
    atomic(path, &serde_json::to_vec_pretty(s)?, 0o600)
}
pub fn cpu_cap(low: u64, high: u64, percent: u8, choices: &[u64]) -> Result<u64> {
    ensure!(low <= high && percent <= 100, "invalid CPU range");
    let target = low + (high - low) * u64::from(percent) / 100;
    Ok(if percent == 100 || choices.is_empty() {
        target
    } else {
        choices
            .iter()
            .copied()
            .filter(|f| *f >= low && *f <= target)
            .max()
            .unwrap_or(low)
    })
}
type Changes = Vec<(PathBuf, String)>;
fn changes(sys: &Path, c: &Config, name: &str) -> Result<Changes> {
    let p = c.profiles.get(name).context("unknown power profile")?;
    let base = sys.join("devices/system/cpu/cpufreq");
    let mut out = Vec::new();
    if p.disable_boost && base.join("boost").exists() {
        out.push((base.join("boost"), "0".into()));
    }
    let mut policies = 0;
    for d in dirs(&base, "policy") {
        if !d.join("scaling_min_freq").exists() {
            continue;
        }
        policies += 1;
        let low = read(d.join("scaling_min_freq"))?.parse::<u64>()?;
        let high = read(d.join("scaling_max_freq"))?.parse::<u64>()?;
        let avail = d.join("scaling_available_frequencies");
        let choices = if avail.exists() {
            read(avail)?
                .split_whitespace()
                .map(str::parse)
                .collect::<std::result::Result<Vec<u64>, _>>()?
        } else {
            vec![]
        };
        out.push((
            d.join("scaling_max_freq"),
            cpu_cap(low, high, p.cpu_percent, &choices)?.to_string(),
        ));
        if p.cpu_percent == 0
            && read(d.join("scaling_available_governors"))?
                .split_whitespace()
                .any(|s| s == "powersave")
        {
            out.push((d.join("scaling_governor"), "powersave".into()));
        }
    }
    ensure!(
        policies > 0 || !out.is_empty(),
        "no supported CPU controls found"
    );
    Ok(out)
}
trait Ops {
    fn inhibit(&mut self) -> Result<()>;
    fn uninhibit(&mut self) -> Result<()>;
    fn service_active(&mut self, user: &str, service: &str) -> Result<bool>;
    fn service_set(&mut self, user: &str, service: &str, on: bool) -> Result<()>;
    fn dpms_state(&mut self, sys: &Path) -> Result<Option<String>>;
    fn dpms(&mut self, sys: &Path, value: &str) -> Result<()>;
    fn set(&mut self, path: &Path, value: &str) -> Result<()> {
        set(path, value)
    }
}
struct Live;
impl Ops for Live {
    fn inhibit(&mut self) -> Result<()> {
        exec(
            "systemd-run",
            &[
                "--quiet",
                "--collect",
                "--unit=prismswitch-inhibit",
                "--property=Type=exec",
                "/usr/bin/systemd-inhibit",
                "--what=sleep:idle",
                "--mode=block",
                "--who=PrismSwitch",
                "--why=Preserve external display and audio",
                "/usr/bin/sleep",
                "infinity",
            ],
            15,
        )?;
        for _ in 0..3 {
            if exec("systemd-inhibit", &["--list", "--no-pager"], 10)?.contains("PrismSwitch") {
                return Ok(());
            }
            pause(1);
        }
        bail!("sleep inhibitor not established")
    }
    fn uninhibit(&mut self) -> Result<()> {
        if active("systemctl", &["is-active", "--quiet", UNIT]) {
            exec("systemctl", &["stop", UNIT], 15)?;
        }
        Ok(())
    }
    fn service_active(&mut self, user: &str, service: &str) -> Result<bool> {
        user_active(user, service)
    }
    fn service_set(&mut self, user: &str, service: &str, on: bool) -> Result<()> {
        if self.service_active(user, service)? != on {
            userctl(user, &[if on { "start" } else { "stop" }, service])?;
        }
        ensure!(
            self.service_active(user, service)? == on,
            "service transition not verified: {service}"
        );
        Ok(())
    }
    fn dpms_state(&mut self, sys: &Path) -> Result<Option<String>> {
        hardware::dpms_state(sys)
    }
    fn dpms(&mut self, sys: &Path, value: &str) -> Result<()> {
        hardware::dpms(sys, value, true)
    }
}
fn restore_with(sys: &Path, path: &Path, ops: &mut impl Ops) -> Result<()> {
    let Some(s) = state(path)? else {
        return Ok(());
    };
    let mut errors = vec![];
    for (p, v) in s.settings.iter().rev() {
        // Runtime state is root-owned; additionally restrict replay to CPU policy paths.
        if !p.starts_with(sys.join("devices/system/cpu/cpufreq")) {
            errors.push(format!("invalid recovery path {}", p.display()));
            continue;
        }
        if let Err(e) = ops.set(p, v) {
            errors.push(format!("{}: {e:#}", p.display()));
        }
    }
    for service in &s.services {
        if !crate::config::service_name(service) {
            errors.push(format!("invalid recovery unit {service}"));
            continue;
        }
        if let Err(e) = ops.service_set(&s.user, service, true) {
            errors.push(format!("{service}: {e:#}"));
        }
    }
    if s.dpms.as_deref() == Some("on") {
        if let Err(e) = ops.dpms(sys, "on") {
            errors.push(format!("DPMS: {e:#}"));
        }
    }
    ensure!(
        errors.is_empty(),
        "rollback incomplete; recovery state retained: {}",
        errors.join("; ")
    );
    ops.uninhibit()?;
    fs::remove_file(path)?;
    println!("Original power and user-service settings restored");
    Ok(())
}
pub fn restore(sys: &Path, path: &Path) -> Result<()> {
    restore_with(sys, path, &mut Live)
}
fn apply_with(sys: &Path, path: &Path, c: &Config, name: &str, ops: &mut impl Ops) -> Result<()> {
    ensure!(!path.exists(), "restore existing power profile first");
    c.validate()?;
    let p = c.profiles.get(name).context("unknown profile")?;
    let ch = changes(sys, c, name)?;
    let mut s = State {
        profile: name.into(),
        active: false,
        user: c.user.clone(),
        services: vec![],
        settings: vec![],
        dpms: None,
    };
    for (p, _) in &ch {
        s.settings.push((p.clone(), read(p)?));
    }
    if c.blank_internal {
        let v = ops
            .dpms_state(sys)?
            .context("no internal DPMS state; disable optional blanking")?;
        ensure!(v == "on" || v == "off", "unsupported DPMS state: {v}");
        s.dpms = Some(v);
    }
    for service in &p.pause_services {
        if ops.service_active(&c.user, service)? {
            s.services.push(service.clone());
        }
    }
    save(path, &s)?;
    let result = (|| -> Result<()> {
        check_interrupt()?;
        ops.inhibit()?;
        for service in &s.services {
            check_interrupt()?;
            ops.service_set(&s.user, service, false)?;
        }
        for (p, v) in &ch {
            check_interrupt()?;
            ops.set(p, v)?;
        }
        if s.dpms.as_deref() == Some("on") {
            check_interrupt()?;
            ops.dpms(sys, "off")?;
        }
        check_interrupt()?;
        s.active = true;
        save(path, &s)?;
        Ok(())
    })();
    if let Err(e) = result {
        if let Err(rollback) = restore_with(sys, path, ops) {
            bail!("activation failed: {e:#}; rollback: {rollback:#}");
        }
        return Err(e);
    }
    println!("Profile {name} active; verify picture and audio");
    Ok(())
}
pub fn apply(sys: &Path, path: &Path, c: &Config, name: &str) -> Result<()> {
    apply_with(sys, path, c, name, &mut Live)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    struct Fixture {
        root: PathBuf,
        sys: PathBuf,
        state: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static ID: AtomicUsize = AtomicUsize::new(0);
            let root = std::env::temp_dir().join(format!(
                "prismswitch-test-{}-{}",
                std::process::id(),
                ID.fetch_add(1, Ordering::Relaxed)
            ));
            let sys = root.join("sys");
            let base = sys.join("devices/system/cpu/cpufreq");
            fs::create_dir_all(base.join("policy0")).unwrap();
            fs::write(base.join("boost"), "1").unwrap();
            for (k, v) in [
                ("scaling_min_freq", "1200"),
                ("scaling_max_freq", "2660"),
                ("scaling_governor", "schedutil"),
                ("scaling_available_governors", "schedutil powersave"),
                ("scaling_available_frequencies", "1200 1600 2000 2660"),
            ] {
                fs::write(base.join("policy0").join(k), v).unwrap();
            }
            let state = root.join("run/eco.json");
            Self { root, sys, state }
        }
        fn cpu(&self, k: &str) -> PathBuf {
            self.sys.join("devices/system/cpu/cpufreq").join(k)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    #[derive(Default)]
    struct Fake {
        inhibited: bool,
        services: BTreeMap<String, bool>,
        dpms: String,
        fail_value: Option<String>,
    }
    impl Ops for Fake {
        fn inhibit(&mut self) -> Result<()> {
            self.inhibited = true;
            Ok(())
        }
        fn uninhibit(&mut self) -> Result<()> {
            self.inhibited = false;
            Ok(())
        }
        fn service_active(&mut self, _: &str, s: &str) -> Result<bool> {
            Ok(*self.services.get(s).unwrap_or(&false))
        }
        fn service_set(&mut self, _: &str, s: &str, on: bool) -> Result<()> {
            self.services.insert(s.into(), on);
            Ok(())
        }
        fn dpms_state(&mut self, _: &Path) -> Result<Option<String>> {
            Ok(Some(self.dpms.clone()))
        }
        fn dpms(&mut self, _: &Path, v: &str) -> Result<()> {
            self.dpms = v.into();
            Ok(())
        }
        fn set(&mut self, p: &Path, v: &str) -> Result<()> {
            if self.fail_value.as_deref() == Some(v) {
                bail!("injected write failure");
            }
            set(p, v)
        }
    }
    fn config() -> Config {
        let mut c = Config::default();
        c.user = "demo".into();
        c.blank_internal = true;
        c.profiles.get_mut("eco").unwrap().pause_services =
            vec!["sync.service".into(), "inactive.service".into()];
        c
    }
    fn fake() -> Fake {
        Fake {
            dpms: "on".into(),
            services: BTreeMap::from([
                ("sync.service".into(), true),
                ("inactive.service".into(), false),
            ]),
            ..Default::default()
        }
    }
    #[test]
    fn frequency() {
        assert_eq!(cpu_cap(1200, 2660, 0, &[1200, 1600, 2660]).unwrap(), 1200);
        assert_eq!(cpu_cap(1200, 2660, 50, &[1200, 1600, 2660]).unwrap(), 1600);
        assert_eq!(cpu_cap(1200, 2660, 100, &[1200, 1600]).unwrap(), 2660);
        assert!(cpu_cap(2000, 1000, 20, &[]).is_err());
    }
    #[test]
    fn apply_rollback() {
        let f = Fixture::new();
        let mut ops = fake();
        apply_with(&f.sys, &f.state, &config(), "eco", &mut ops).unwrap();
        assert!(state(&f.state).unwrap().unwrap().active);
        assert!(ops.inhibited);
        assert!(!ops.services["sync.service"]);
        assert_eq!(ops.dpms, "off");
        assert_eq!(read(f.cpu("boost")).unwrap(), "0");
        assert_eq!(read(f.cpu("policy0/scaling_max_freq")).unwrap(), "1200");
        restore_with(&f.sys, &f.state, &mut ops).unwrap();
        assert!(ops.services["sync.service"]);
        assert!(!ops.services["inactive.service"]);
        assert_eq!(ops.dpms, "on");
        assert!(!ops.inhibited);
        assert!(!f.state.exists());
        assert_eq!(read(f.cpu("policy0/scaling_max_freq")).unwrap(), "2660");
        assert_eq!(
            read(f.cpu("policy0/scaling_governor")).unwrap(),
            "schedutil"
        );
    }
    #[test]
    fn failed_apply_reverts() {
        let f = Fixture::new();
        let mut ops = fake();
        ops.fail_value = Some("powersave".into());
        assert!(apply_with(&f.sys, &f.state, &config(), "eco", &mut ops).is_err());
        assert!(!f.state.exists());
        assert_eq!(read(f.cpu("boost")).unwrap(), "1");
        assert!(ops.services["sync.service"]);
        assert!(!ops.inhibited);
    }
    #[test]
    fn failed_restore_preserves_state() {
        let f = Fixture::new();
        let mut ops = fake();
        apply_with(&f.sys, &f.state, &config(), "eco", &mut ops).unwrap();
        ops.fail_value = Some("2660".into());
        assert!(restore_with(&f.sys, &f.state, &mut ops).is_err());
        assert!(f.state.exists());
        assert!(ops.inhibited);
        ops.fail_value = None;
        restore_with(&f.sys, &f.state, &mut ops).unwrap();
        assert!(!f.state.exists());
    }
    #[test]
    fn no_double_apply() {
        let f = Fixture::new();
        let mut ops = fake();
        apply_with(&f.sys, &f.state, &config(), "eco", &mut ops).unwrap();
        assert!(apply_with(&f.sys, &f.state, &config(), "eco", &mut ops).is_err());
        restore_with(&f.sys, &f.state, &mut ops).unwrap();
    }
    #[test]
    fn no_state_restore() {
        let f = Fixture::new();
        restore_with(&f.sys, &f.state, &mut fake()).unwrap();
    }
}
