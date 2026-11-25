// SPDX-License-Identifier: GPL-2.0-only
use anyhow::{bail, ensure, Context, Result};
use std::{
    ffi::OsStr,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
static INTERRUPTED: AtomicBool = AtomicBool::new(false);
extern "C" fn interrupted(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::Relaxed);
}
pub fn signals() {
    unsafe {
        libc::signal(
            libc::SIGTERM,
            interrupted as *const () as libc::sighandler_t,
        );
        libc::signal(libc::SIGINT, interrupted as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, interrupted as *const () as libc::sighandler_t);
    }
}
pub fn check_interrupt() -> Result<()> {
    ensure!(
        !INTERRUPTED.load(Ordering::Relaxed),
        "interrupted; reverting incomplete transaction"
    );
    Ok(())
}
pub fn pause(seconds: u64) {
    std::thread::sleep(Duration::from_secs(seconds));
}
pub fn root() -> bool {
    unsafe { libc::geteuid() == 0 }
}
pub fn read(p: impl AsRef<Path>) -> Result<String> {
    let p = p.as_ref();
    Ok(fs::read_to_string(p)
        .with_context(|| format!("read {}", p.display()))?
        .trim()
        .to_owned())
}
pub fn write(p: impl AsRef<Path>, v: impl AsRef<str>) -> Result<()> {
    let p = p.as_ref();
    fs::write(p, format!("{}\n", v.as_ref())).with_context(|| format!("write {}", p.display()))
}
pub fn set(p: &Path, v: &str) -> Result<()> {
    if read(p)? != v {
        write(p, v)?;
        ensure!(read(p)? == v, "readback mismatch: {}", p.display());
    }
    Ok(())
}
pub fn dirs(p: &Path, prefix: &str) -> Vec<PathBuf> {
    let mut v = fs::read_dir(p)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
        .map(|e| e.path())
        .collect::<Vec<_>>();
    v.sort();
    v
}
pub fn atomic(path: &Path, data: &[u8], mode: u32) -> Result<()> {
    let parent = path.parent().context("missing parent")?;
    if !parent.exists() {
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    let suffix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let temp = parent.join(format!(".prismswitch-{}-{suffix}", std::process::id()));
    let result = (|| -> Result<()> {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&temp)?;
        f.write_all(data)?;
        f.sync_all()?;
        fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}
pub fn lock(path: &Path) -> Result<std::fs::File> {
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let rc = unsafe {
        libc::flock(
            std::os::fd::AsRawFd::as_raw_fd(&file),
            libc::LOCK_EX | libc::LOCK_NB,
        )
    };
    ensure!(rc == 0, "another PrismSwitch command is running");
    Ok(file)
}
pub fn exec<S: AsRef<OsStr>>(program: &str, args: &[S], seconds: u64) -> Result<String> {
    let out = Command::new("/usr/bin/timeout")
        .args(["--kill-after=2s", &format!("{seconds}s"), program])
        .args(args)
        .output()
        .with_context(|| format!("launch {program}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        bail!(
            "{program}: {} {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(stdout.trim().into())
}
pub fn active(program: &str, args: &[&str]) -> bool {
    exec(program, args, 15).is_ok()
}
pub fn user_command(user: &str, tail: &[&str]) -> Result<Vec<String>> {
    let uid = exec("id", &["-u", user], 5)?.parse::<u32>()?;
    ensure!(uid != 0, "root user services are not supported");
    let runtime = format!("/run/user/{uid}");
    let mut args = vec![
        "-u".into(),
        user.into(),
        "--".into(),
        "env".into(),
        format!("XDG_RUNTIME_DIR={runtime}"),
        format!("DBUS_SESSION_BUS_ADDRESS=unix:path={runtime}/bus"),
    ];
    args.extend(tail.iter().map(|s| s.to_string()));
    Ok(args)
}
pub fn userctl(user: &str, tail: &[&str]) -> Result<String> {
    let mut cmd = vec!["systemctl", "--user"];
    cmd.extend(tail);
    exec("runuser", &user_command(user, &cmd)?, 30)
}
pub fn user_active(user: &str, service: &str) -> Result<bool> {
    let args = user_command(
        user,
        &[
            "systemctl",
            "--user",
            "show",
            "-p",
            "ActiveState",
            "--value",
            service,
        ],
    )?;
    let value = exec("runuser", &args, 15)?;
    match value.as_str() {
        "active" => Ok(true),
        "inactive" | "failed" => Ok(false),
        _ => bail!("user unit {service} is in transitional state: {value}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exclusive_lock() {
        let path =
            std::env::temp_dir().join(format!("prismswitch-lock-test-{}", std::process::id()));
        let one = lock(&path).unwrap();
        assert!(lock(&path).is_err());
        drop(one);
        let two = lock(&path).unwrap();
        drop(two);
        fs::remove_file(path).unwrap();
    }
}
