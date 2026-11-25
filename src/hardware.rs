// SPDX-License-Identifier: GPL-2.0-only
use crate::{config::Config, os::*};
use anyhow::{bail, ensure, Context, Result};
use std::{
    fs,
    path::{Path, PathBuf},
};
pub const SMC: &str = "/usr/local/libexec/prismswitch/SmcDumpKey";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Internal,
    External,
}
impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Internal => "off",
            Self::External => "on",
        }
    }
    pub fn flip(self) -> Self {
        match self {
            Self::Internal => Self::External,
            Self::External => Self::Internal,
        }
    }
}
pub fn panels(sys: &Path) -> Vec<PathBuf> {
    dirs(&sys.join("class/drm"), "card")
        .into_iter()
        .filter(|p| {
            let s = p.file_name().unwrap_or_default().to_string_lossy();
            s.contains("-eDP-") || s.contains("-LVDS-")
        })
        .collect()
}
pub fn edid_present(sys: &Path) -> bool {
    panels(sys)
        .iter()
        .any(|p| fs::read(p.join("edid")).is_ok_and(|x| x.len() >= 128))
}
pub fn device(sys: &Path, c: &Config) -> Result<PathBuf> {
    let model = read(sys.join("class/dmi/id/product_name"))?;
    ensure!(
        model == "iMac11,1"
            || (c.allow_experimental && ["iMac11,3", "iMac10,1"].contains(&model.as_str())),
        "{model}: unsupported or experimental; see compatibility guide"
    );
    if model == "iMac10,1" {
        ensure!(
            panels(sys)
                .iter()
                .any(|p| read(p.join("modes")).is_ok_and(|s| s.lines().any(|l| l == "2560x1440"))),
            "iMac10,1 requires detected native 27-inch panel"
        );
    }
    let ds = dirs(&sys.join("bus/platform/drivers/applesmc"), "applesmc.");
    ensure!(
        ds.len() == 1 && ds[0].join("key_at_index").exists(),
        "exactly one bound applesmc device with key-index ABI required"
    );
    Ok(ds[0].clone())
}
pub fn native(dev: &Path) -> Result<Mode> {
    let index = dev.join("key_at_index");
    let old = read(&index)?;
    let result = (|| -> Result<Mode> {
        let count = read(dev.join("key_count"))?.parse::<u32>()?;
        ensure!(count > 0 && count <= 16384, "invalid SMC key count");
        for n in 0..count {
            write(&index, n.to_string())?;
            if read(dev.join("key_at_index_name"))? == "MVHR" {
                return match fs::read(dev.join("key_at_index_data"))?.as_slice() {
                    [0] => Ok(Mode::Internal),
                    [1] => Ok(Mode::External),
                    _ => bail!("unexpected MVHR value"),
                };
            }
        }
        bail!("MVHR key not found")
    })();
    write(&index, old).context("restore shared SMC key selector")?;
    result
}
fn smc(args: &[&str]) -> Result<String> {
    use std::process::Command;
    let out = Command::new("/usr/bin/timeout")
        .args(["--kill-after=1s", "5s", SMC])
        .args(args)
        .output()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let all = format!("{}{}", stdout, String::from_utf8_lossy(&out.stderr));
    ensure!(
        out.status.success()
            && !all.to_ascii_lowercase().contains("fail")
            && !all.to_ascii_lowercase().contains("error"),
        "SMC transaction: {all}"
    );
    Ok(stdout.into_owned())
}
fn raw_mode(text: &str) -> Result<Mode> {
    if text.contains(r#"data="\x00""#) {
        Ok(Mode::Internal)
    } else if text.contains(r#"data="\x01""#) {
        Ok(Mode::External)
    } else {
        bail!("invalid MVHR reply")
    }
}
fn fan_floors(sys: &Path) -> Result<Vec<(String, String)>> {
    let mut fans = Vec::new();
    for h in dirs(&sys.join("class/hwmon"), "hwmon") {
        if read(h.join("name")).ok().as_deref() != Some("applesmc") {
            continue;
        }
        for i in 1..=8 {
            let min = h.join(format!("fan{i}_min"));
            if !min.exists() {
                continue;
            }
            let pwm = h.join(format!("pwm{i}_enable"));
            let manual = h.join(format!("fan{i}_manual"));
            if pwm.exists() {
                ensure!(read(&pwm)? == "2", "SMC fan {i} not in automatic mode");
            } else if manual.exists() {
                ensure!(read(&manual)? == "0", "SMC fan {i} not in automatic mode");
            } else {
                bail!("cannot verify automatic SMC fan control");
            }
            let label = read(h.join(format!("fan{i}_label")))?;
            fans.push((label, read(&min)?));
        }
    }
    ensure!(!fans.is_empty(), "cannot verify SMC fan controls");
    fans.sort();
    Ok(fans)
}
fn verify_fans(sys: &Path, original: &[(String, String)]) -> Result<()> {
    let current = fan_floors(sys)?;
    ensure!(
        current.len() == original.len(),
        "SMC fan enumeration changed"
    );
    for ((label, min), (old_label, old_min)) in current.iter().zip(original) {
        ensure!(label == old_label, "SMC fan labels changed");
        ensure!(
            min.parse::<u32>()? >= old_min.parse::<u32>()?,
            "SMC fan minimum dropped for {label}"
        );
    }
    Ok(())
}
struct Rebind {
    driver: PathBuf,
    dev: PathBuf,
}
impl Rebind {
    fn restore(&self) -> Result<()> {
        if !self.dev.exists() {
            fs::write(
                self.driver.join("bind"),
                self.dev
                    .file_name()
                    .context("device name")?
                    .as_encoded_bytes(),
            )
            .context("CRITICAL: applesmc rebind failed")?;
        }
        ensure!(self.dev.exists(), "CRITICAL: applesmc not bound");
        Ok(())
    }
}
impl Drop for Rebind {
    fn drop(&mut self) {
        if let Err(e) = self.restore() {
            eprintln!("{e:#}");
        }
    }
}
pub fn route(sys: &Path, dev: &Path, wanted: Mode) -> Result<()> {
    ensure!(
        Path::new(SMC).exists(),
        "upstream SMC helper is not installed"
    );
    if wanted == Mode::External {
        ensure!(
            edid_present(sys),
            "internal panel EDID missing; recover internal route first"
        );
    }
    let fans = fan_floors(sys)?;
    let driver = dev.parent().context("SMC driver")?.to_path_buf();
    let guard = Rebind {
        driver: driver.clone(),
        dev: dev.to_path_buf(),
    };
    let result = (|| -> Result<()> {
        check_interrupt()?;
        fs::write(
            driver.join("unbind"),
            dev.file_name().context("device name")?.as_encoded_bytes(),
        )?;
        for attempt in 1..=4 {
            check_interrupt()?;
            let value = if wanted == Mode::External { "1" } else { "0" };
            if let Err(e) = smc(&["MVHR", value]) {
                eprintln!("SMC request acknowledgement uncertain (attempt {attempt}): {e}");
            }
            pause(1);
            if let Err(e) = smc(&["MVMR", "2"]) {
                eprintln!("SMC apply acknowledgement uncertain (attempt {attempt}): {e}");
            }
            pause(2);
            if smc(&["MVHR"]).and_then(|s| raw_mode(&s)).ok() == Some(wanted) {
                return Ok(());
            }
        }
        bail!("SMC route not confirmed")
    })();
    guard.restore()?;
    verify_fans(sys, &fans)?;
    result?;
    ensure!(native(dev)? == wanted, "native SMC route readback mismatch");
    if wanted == Mode::Internal {
        for p in panels(sys) {
            if p.join("status").exists() {
                let _ = write(p.join("status"), "detect");
            }
        }
        pause(2);
        dpms(sys, "on", false)?;
        ensure!(
            edid_present(sys),
            "internal route requested but panel EDID missing"
        );
    }
    println!(
        "Route requested: {}; visual confirmation required",
        wanted.as_str()
    );
    Ok(())
}
pub fn dpms_state(sys: &Path) -> Result<Option<String>> {
    let states = panels(sys)
        .iter()
        .filter(|p| p.join("dpms").exists())
        .map(|p| read(p.join("dpms")).map(|x| x.to_ascii_lowercase()))
        .collect::<Result<Vec<_>>>()?;
    if states.is_empty() {
        return Ok(None);
    }
    ensure!(
        states.iter().all(|s| s == &states[0]),
        "inconsistent internal DPMS state"
    );
    Ok(Some(states[0].clone()))
}
pub fn dpms(sys: &Path, value: &str, required: bool) -> Result<()> {
    if dpms_state(sys)?.as_deref() == Some(value) {
        return Ok(());
    }
    let sid = exec(
        "loginctl",
        &["show-seat", "seat0", "-p", "ActiveSession", "--value"],
        5,
    )?;
    if sid.is_empty() {
        ensure!(!required, "no active desktop for DPMS");
        return Ok(());
    }
    let prop = |name| {
        exec(
            "loginctl",
            &["show-session", &sid, "-p", name, "--value"],
            5,
        )
    };
    let typ = prop("Type")?;
    let user = prop("Name")?;
    ensure!(!user.is_empty(), "no active desktop user");
    let uid = exec("id", &["-u", &user], 5)?;
    if typ == "wayland" {
        use std::os::unix::fs::FileTypeExt;
        let runtime = PathBuf::from(format!("/run/user/{uid}"));
        let sockets = dirs(&runtime, "wayland-")
            .into_iter()
            .filter(|p| fs::metadata(p).is_ok_and(|m| m.file_type().is_socket()))
            .collect::<Vec<_>>();
        ensure!(sockets.len() == 1, "no unique Wayland socket");
        let mut args = user_command(&user, &[])?;
        args.extend([
            format!(
                "WAYLAND_DISPLAY={}",
                sockets[0].file_name().unwrap().to_string_lossy()
            ),
            "QT_QPA_PLATFORM=wayland".into(),
            "kscreen-doctor".into(),
            "--dpms".into(),
            value.into(),
        ]);
        exec("runuser", &args, 10)?;
    } else if typ == "x11" {
        let display = prop("Display")?;
        let passwd = exec("getent", &["passwd", &user], 5)?;
        let home = passwd.split(':').nth(5).context("user home")?;
        let mut args = user_command(&user, &[])?;
        args.extend([
            format!("DISPLAY={display}"),
            format!("XAUTHORITY={home}/.Xauthority"),
            "xset".into(),
            "dpms".into(),
            "force".into(),
            value.into(),
        ]);
        exec("runuser", &args, 10)?;
    } else {
        ensure!(!required, "unsupported session type for DPMS: {typ}");
        return Ok(());
    }
    pause(1);
    ensure!(
        dpms_state(sys)?.as_deref() == Some(value),
        "internal DPMS {value} not verified"
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn raw() {
        assert_eq!(raw_mode(r#"data="\x01""#).unwrap(), Mode::External);
        assert_eq!(raw_mode(r#"data="\x00""#).unwrap(), Mode::Internal);
        assert!(raw_mode("nonsense").is_err());
    }
    #[test]
    fn flip() {
        assert_eq!(Mode::External.flip(), Mode::Internal);
    }
}

#[cfg(test)]
mod hardware_fixture_tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static ID: AtomicUsize = AtomicUsize::new(0);
            let p = std::env::temp_dir().join(format!(
                "prismswitch-hw-{}-{}",
                std::process::id(),
                ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn put(&self, path: &str, data: &[u8]) {
            let p = self.0.join(path);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, data).unwrap();
        }
        fn native_fixture(&self, byte: u8) -> PathBuf {
            self.put("smc/key_at_index", b"4");
            self.put("smc/key_count", b"5");
            self.put("smc/key_at_index_name", b"MVHR");
            self.put("smc/key_at_index_data", &[byte]);
            self.0.join("smc")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn selector_restored() {
        let f = Fixture::new();
        let dev = f.native_fixture(1);
        assert_eq!(native(&dev).unwrap(), Mode::External);
        assert_eq!(read(dev.join("key_at_index")).unwrap(), "4");
    }
    #[test]
    fn selector_restored_on_invalid_read() {
        let f = Fixture::new();
        let dev = f.native_fixture(2);
        assert!(native(&dev).is_err());
        assert_eq!(read(dev.join("key_at_index")).unwrap(), "4");
    }
    #[test]
    fn rejects_unsupported() {
        let f = Fixture::new();
        f.put("class/dmi/id/product_name", b"iMac18,3");
        assert!(device(&f.0, &Config::default()).is_err());
    }
    #[test]
    fn requires_opt_in_and_panel() {
        let f = Fixture::new();
        f.put("class/dmi/id/product_name", b"iMac10,1");
        f.put(
            "bus/platform/drivers/applesmc/applesmc.768/key_at_index",
            b"0",
        );
        let mut c = Config::default();
        assert!(device(&f.0, &c).is_err());
        c.allow_experimental = true;
        assert!(device(&f.0, &c).is_err());
        f.put("class/drm/card1-eDP-1/modes", b"2560x1440\n");
        assert!(device(&f.0, &c).is_ok());
    }
    #[test]
    fn checks_automatic_fans_and_minimums() {
        let f = Fixture::new();
        f.put("class/hwmon/hwmon0/name", b"applesmc");
        f.put("class/hwmon/hwmon0/fan1_label", b"CPU ");
        f.put("class/hwmon/hwmon0/fan1_min", b"1200");
        f.put("class/hwmon/hwmon0/pwm1_enable", b"2");
        let original = fan_floors(&f.0).unwrap();
        verify_fans(&f.0, &original).unwrap();
        f.put("class/hwmon/hwmon0/fan1_min", b"1000");
        assert!(verify_fans(&f.0, &original).is_err());
        f.put("class/hwmon/hwmon0/pwm1_enable", b"1");
        assert!(fan_floors(&f.0).is_err());
    }
    #[test]
    fn rebind_guard_attempts_recovery() {
        let f = Fixture::new();
        f.put("driver/bind", b"");
        let driver = f.0.join("driver");
        {
            let _guard = Rebind {
                driver: driver.clone(),
                dev: driver.join("applesmc.768"),
            };
        }
        assert_eq!(fs::read(driver.join("bind")).unwrap(), b"applesmc.768");
    }
}
