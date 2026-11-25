#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Local Linux iMac Target Display Mode controller."""
import argparse
import contextlib
import copy
import fcntl
import json
import os
import pwd
import re
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

VERSION = "0.1.0"
CONFIG = Path("/etc/prismswitch/config.json")
STATE = Path("/run/prismswitch/eco.json")
LOCK = Path("/run/prismswitch.lock")
SMC = "/usr/local/libexec/prismswitch/SmcDumpKey"
SYS = Path("/sys")
DRIVER = SYS / "bus/platform/drivers/applesmc"
KEYD = Path("/etc/keyd/prismswitch.conf")
UNIT = "prismswitch-inhibit.service"
DEFAULT = {
    "allow_experimental": False, "blank_internal": False, "user": "",
    "shortcuts": {"toggle": "Ctrl+Alt+M", "eco": "Ctrl+Alt+E"},
    "profiles": {"eco": {"cpu_percent": 0, "disable_boost": True, "pause_services": []}}
}
MODS = ("control", "alt", "shift", "meta")
PROFILE = re.compile(r"[a-z][a-z0-9_-]{0,31}\Z")
SERVICE = re.compile(r"[A-Za-z0-9_][A-Za-z0-9_.@-]*\.service\Z")

class Error(RuntimeError):
    pass

def run(*args, check=True, timeout=15):
    p = subprocess.run([str(a) for a in args], text=True, capture_output=True, timeout=timeout)
    if check and p.returncode:
        raise Error(f"{args[0]}: {p.stderr.strip() or p.stdout.strip() or p.returncode}")
    return p.stdout.strip()

def success(*args):
    return subprocess.run([str(a) for a in args], stdout=subprocess.DEVNULL,
                          stderr=subprocess.DEVNULL, timeout=15).returncode == 0

def read(path):
    return Path(path).read_text().strip()

def write(path, value):
    Path(path).write_text(str(value) + "\n")

def atomic(path, text):
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    fd, name = tempfile.mkstemp(dir=path.parent, prefix=".prismswitch-")
    try:
        with os.fdopen(fd, "w") as f:
            f.write(text)
            f.flush()
            os.fsync(f.fileno())
        os.replace(name, path)
    finally:
        Path(name).unlink(missing_ok=True)

def save(path, obj):
    atomic(path, json.dumps(obj, indent=2, ensure_ascii=False) + "\n")

def chord(value):
    if not isinstance(value, str):
        raise Error("Shortcut must be a string")
    aliases = {"ctrl": "control", "super": "meta", "win": "meta"}
    parts = [aliases.get(p.lower(), p.lower()) for p in value.split("+")]
    if len(parts) < 3 or len(set(parts)) != len(parts) or any(p not in MODS for p in parts[:-1]):
        raise Error("Use two or more distinct modifiers, e.g. Ctrl+Alt+E")
    if not re.fullmatch(r"[a-z0-9]|f(?:[1-9]|1[0-2])", parts[-1]):
        raise Error("Use a letter, digit or F1–F12")
    return "+".join(m for m in MODS if m in parts[:-1]), parts[-1]

def validate(c):
    if not isinstance(c, dict) or set(c) != set(DEFAULT):
        raise Error("Invalid top-level configuration fields")
    if any(type(c[k]) is not bool for k in ("allow_experimental", "blank_internal")):
        raise Error("Display/model options must be booleans")
    if not isinstance(c["user"], str) or (c["user"] and not re.fullmatch(r"[a-zA-Z_][a-zA-Z0-9_.-]*", c["user"])):
        raise Error("Invalid local username")
    if not isinstance(c["profiles"], dict) or not isinstance(c["shortcuts"], dict):
        raise Error("Profiles and shortcuts must be objects")
    if not c["profiles"] or "toggle" not in c["shortcuts"]:
        raise Error("Define at least one profile and a normal toggle shortcut")
    for name, p in c["profiles"].items():
        if not PROFILE.fullmatch(name) or name in {"toggle", "on", "off", "status", "configure", "kick", "restore", "profile"}:
            raise Error(f"Invalid profile: {name}")
        if not isinstance(p, dict) or set(p) != {"cpu_percent", "disable_boost", "pause_services"}:
            raise Error(f"Invalid profile fields: {name}")
        if type(p["cpu_percent"]) is not int or not 0 <= p["cpu_percent"] <= 100 or type(p["disable_boost"]) is not bool:
            raise Error("CPU percentage must be 0..100; boost option must be boolean")
        services = p["pause_services"]
        if not isinstance(services, list) or any(not isinstance(x, str) or not SERVICE.fullmatch(x) for x in services):
            raise Error("Only named user systemd .service units may be paused")
        if len(set(services)) != len(services) or (services and not c["user"]):
            raise Error("Services must be unique; explicitly set a non-root user")
    seen = set()
    for action, value in c["shortcuts"].items():
        if action != "toggle" and action not in c["profiles"]:
            raise Error(f"Unknown shortcut action: {action}")
        k = chord(value)
        if k in seen:
            raise Error("Duplicate shortcut")
        seen.add(k)
    return c

def load():
    return validate(json.loads(CONFIG.read_text()) if CONFIG.exists() else copy.deepcopy(DEFAULT))

def keyd_config(c):
    validate(c)
    groups = {}
    for action, value in c["shortcuts"].items():
        layer, key = chord(value)
        command = "toggle" if action == "toggle" else f"profile {action}"
        groups.setdefault(layer, []).append(f"{key} = command(/usr/local/bin/prismswitch {command})")
    lines = ["# PrismSwitch — unbound keys retain their normal behavior.", "[ids]", "*"]
    for layer, bindings in sorted(groups.items()):
        lines += ["", f"[{layer}]", *bindings]
    return "\n".join(lines) + "\n"

def cpu_cap(low, high, percent, available=()):
    target = low + (high - low) * percent // 100
    if available:
        return max((f for f in available if low <= f <= target), default=low)
    return target

def panel_paths():
    return list((SYS / "class/drm").glob("card*-eDP-*")) + list((SYS / "class/drm").glob("card*-LVDS-*"))

def device(c):
    model = read(SYS / "class/dmi/id/product_name")
    if model != "iMac11,1" and not (c["allow_experimental"] and model in {"iMac11,3", "iMac10,1"}):
        raise Error(f"{model}: unsupported or experimental; see compatibility notes")
    if model == "iMac10,1" and not any("2560x1440" in read(p / "modes") for p in panel_paths() if (p / "modes").exists()):
        raise Error("iMac10,1 requires a detected 27-inch native 2560×1440 panel")
    ds = list(DRIVER.glob("applesmc.*"))
    if len(ds) != 1 or not (ds[0] / "key_at_index").exists():
        raise Error("Exactly one bound applesmc device with key-index ABI is required")
    return ds[0]

def mode_native(dev):
    idx = dev / "key_at_index"
    original = read(idx)
    try:
        for n in range(int(read(dev / "key_count"))):
            write(idx, n)
            if read(dev / "key_at_index_name") == "MVHR":
                value = (dev / "key_at_index_data").read_bytes()
                if value in (b"\x00", b"\x01"):
                    return "on" if value == b"\x01" else "off"
                raise Error("Unexpected MVHR value")
        raise Error("SMC has no MVHR key")
    finally:
        write(idx, original)

def smc(*args):
    p = subprocess.run([SMC, *args], text=True, capture_output=True, timeout=5)
    out = p.stdout + p.stderr
    if p.returncode or re.search("fail|error", out, re.I):
        raise Error(out.strip() or "SMC helper failed")
    return p.stdout

def mode_raw():
    m = re.search(r'data="\\x(00|01)"', smc("MVHR"))
    if not m:
        raise Error("Invalid raw MVHR reply")
    return "on" if m[1] == "01" else "off"

def route(dev, wanted):
    # Always rebind the kernel SMC driver, even on a failed write.
    # Readback is routing-request evidence, NOT proof of visible pixels.
    if not os.path.exists(SMC):
        raise Error("Install the upstream SMC helper first")
    confirmed = False
    try:
        (DRIVER / "unbind").write_text(dev.name)
        for attempt in range(4):
            try:
                smc("MVHR", "1" if wanted == "on" else "0")
            except (Error, subprocess.TimeoutExpired) as e:
                print(f"SMC request acknowledgement uncertain: {e}", file=sys.stderr)
            time.sleep(1)
            try:
                smc("MVMR", "2")
            except (Error, subprocess.TimeoutExpired) as e:
                print(f"SMC apply acknowledgement uncertain: {e}", file=sys.stderr)
            time.sleep(2)
            try:
                if mode_raw() == wanted:
                    confirmed = True
                    break
            except (Error, subprocess.TimeoutExpired):
                pass
    finally:
        if not dev.exists():
            (DRIVER / "bind").write_text(dev.name)
        if not dev.exists():
            raise Error("CRITICAL: applesmc rebind failed")
    if not confirmed or mode_native(dev) != wanted:
        raise Error("SMC routing request not confirmed")
    if wanted == "off":
        for p in panel_paths():
            with contextlib.suppress(OSError):
                write(p / "status", "detect")
        time.sleep(2)
        try:
            dpms("on")
        except (OSError, Error, subprocess.TimeoutExpired) as e:
            print(f"Internal route requested, DPMS wake not verified: {e}", file=sys.stderr)
            raise
    print(f"Route requested: {wanted}; visual confirmation required")

def dpms(value):
    # KDE Wayland (including its greeter) and X11; no compositor is also valid.
    sid = run("loginctl", "show-seat", "seat0", "-p", "ActiveSession", "--value", check=False)
    if not sid:
        return
    typ = run("loginctl", "show-session", sid, "-p", "Type", "--value", check=False)
    user = run("loginctl", "show-session", sid, "-p", "Name", "--value", check=False)
    if not user:
        return
    entry = pwd.getpwnam(user)
    runtime = Path(f"/run/user/{entry.pw_uid}")
    if typ == "wayland":
        sockets = [x for x in runtime.glob("wayland-[0-9]*") if x.is_socket()]
        if len(sockets) != 1:
            raise Error("No unique Wayland socket; DPMS action unavailable")
        run("runuser", "-u", user, "--", "env", f"XDG_RUNTIME_DIR={runtime}",
            f"DBUS_SESSION_BUS_ADDRESS=unix:path={runtime}/bus", f"WAYLAND_DISPLAY={sockets[0].name}",
            "QT_QPA_PLATFORM=wayland", "kscreen-doctor", "--dpms", value, timeout=10)
    elif typ == "x11":
        display = run("loginctl", "show-session", sid, "-p", "Display", "--value", check=False)
        if not display:
            raise Error("No X11 display")
        run("runuser", "-u", user, "--", "env", f"DISPLAY={display}",
            f"XAUTHORITY={entry.pw_dir}/.Xauthority", "xset", "dpms", "force", value)
    else:
        return
    time.sleep(1)
    states = [read(p / "dpms").lower() for p in panel_paths() if (p / "dpms").exists()]
    if states and any(s != value for s in states):
        raise Error(f"Internal DPMS {value} could not be verified")

def userctl(user, *args):
    entry = pwd.getpwnam(user)
    if entry.pw_uid == 0:
        raise Error("Pausing root services is prohibited")
    runtime = f"/run/user/{entry.pw_uid}"
    return ["runuser", "-u", user, "--", "env", f"XDG_RUNTIME_DIR={runtime}",
            f"DBUS_SESSION_BUS_ADDRESS=unix:path={runtime}/bus", "systemctl", "--user", *args]

def set_checked(path, value):
    write(path, value)
    if read(path) != str(value):
        raise Error(f"Readback mismatch: {path}")

def restore():
    if not STATE.exists():
        return
    state = json.loads(STATE.read_text())
    errors = []
    for path, value in state["settings"]:
        try:
            if read(path) != value:
                set_checked(Path(path), value)
        except (OSError, Error) as e:
            errors.append(str(e))
    for service in state["services"]:
        try:
            run(*userctl(state["user"], "start", service))
            if not success(*userctl(state["user"], "is-active", "--quiet", service)):
                raise Error(f"Service did not restart: {service}")
        except (OSError, Error, subprocess.TimeoutExpired) as e:
            errors.append(str(e))
    if state["dpms"] == "on":
        try:
            dpms("on")
        except (OSError, Error, subprocess.TimeoutExpired) as e:
            errors.append(str(e))
    if errors:
        raise Error("Rollback incomplete; retry restore: " + "; ".join(errors))
    if success("systemctl", "is-active", "--quiet", UNIT):
        run("systemctl", "stop", UNIT)
    STATE.unlink()
    print("Original power and service settings restored")

def apply_profile(c, name):
    p = c["profiles"][name]
    state = {"profile": name, "active": False, "user": c["user"], "services": [], "settings": [], "dpms": ""}
    changes = []
    boost = SYS / "devices/system/cpu/cpufreq/boost"
    if p["disable_boost"] and boost.exists():
        changes.append((boost, "0"))
    for d in sorted((SYS / "devices/system/cpu/cpufreq").glob("policy*")):
        if not (d / "scaling_min_freq").exists():
            continue
        low, high = int(read(d / "scaling_min_freq")), int(read(d / "scaling_max_freq"))
        avail = d / "scaling_available_frequencies"
        choices = [int(x) for x in read(avail).split()] if avail.exists() else []
        changes.append((d / "scaling_max_freq", str(cpu_cap(low, high, p["cpu_percent"], choices))))
        if p["cpu_percent"] == 0 and "powersave" in read(d / "scaling_available_governors").split():
            changes.append((d / "scaling_governor", "powersave"))
    if not changes:
        raise Error("No supported CPU controls found; no changes made")
    state["settings"] = [(str(path), read(path)) for path, _ in changes]
    if c["blank_internal"]:
        panel = next((d / "dpms" for d in panel_paths() if (d / "dpms").exists()), None)
        state["dpms"] = read(panel).lower() if panel else ""
    for service in p["pause_services"]:
        if success(*userctl(c["user"], "is-active", "--quiet", service)):
            state["services"].append(service)
    save(STATE, state)
    try:
        run("systemd-run", "--quiet", "--collect", "--unit=" + UNIT.removesuffix(".service"),
            "--property=Type=exec", "/usr/bin/systemd-inhibit", "--what=sleep:idle", "--mode=block",
            "--who=PrismSwitch", "--why=Preserve external display and audio", "/usr/bin/sleep", "infinity")
        if "PrismSwitch" not in run("systemd-inhibit", "--list", "--no-pager"):
            time.sleep(1)
            if "PrismSwitch" not in run("systemd-inhibit", "--list", "--no-pager"):
                raise Error("Sleep inhibitor not established")
        for service in state["services"]:
            run(*userctl(c["user"], "stop", service))
            if success(*userctl(c["user"], "is-active", "--quiet", service)):
                raise Error(f"Service did not stop: {service}")
        for path, value in changes:
            if read(path) != value:
                set_checked(path, value)
        if c["blank_internal"] and state["dpms"] == "on":
            dpms("off")
        state["active"] = True
        save(STATE, state)
    except BaseException:
        restore()
        raise
    print(f"Profile {name} active; verify picture and audio")

def configure(c, hotkeys=True):
    if STATE.exists():
        raise Error("Restore current power profile before configuring")
    if not sys.stdin.isatty():
        raise Error("Interactive terminal required")
    print("PrismSwitch | Two inputs. One screen.")
    c = copy.deepcopy(c)
    print("Current profiles:", ", ".join(c["profiles"]))
    while True:
        name = input("Add a profile (name, or Enter to continue): ").strip()
        if not name:
            break
        if not PROFILE.fullmatch(name) or name in c["profiles"] or name in {"toggle", "on", "off", "status", "configure", "kick", "restore", "profile"}:
            print("Invalid or already used profile name")
            continue
        c["profiles"][name] = {"cpu_percent": 50, "disable_boost": True, "pause_services": []}
    for action in ("toggle", *c["profiles"]):
        old = c["shortcuts"].get(action, "")
        v = input(f"{action} shortcut [{old or 'none'}] (enter to keep, - to clear): ").strip()
        if v == "-":
            if action == "toggle":
                raise Error("Keep a normal toggle shortcut")
            c["shortcuts"].pop(action, None)
        elif v:
            c["shortcuts"][action] = v
    v = input(f"Local account for optional user-service pausing [{c['user'] or 'none'}]: ").strip()
    if v:
        c["user"] = v
    if c["user"] and pwd.getpwnam(c["user"]).pw_uid == 0:
        raise Error("Root account is not supported")
    for name, p in c["profiles"].items():
        v = input(f"{name} CPU range percentage 0=min, 100=normal [{p['cpu_percent']}]: ").strip()
        if v:
            p["cpu_percent"] = int(v)
        v = input(f"{name} disable boost? [current {p['disable_boost']}, y/n]: ").strip().lower()
        if v in ("y", "n"):
            p["disable_boost"] = v == "y"
        if c["user"]:
            old = ",".join(p["pause_services"])
            v = input(f"{name} user services to pause, comma-separated [{old or 'none'}], - to clear: ").strip()
            if v == "-":
                p["pause_services"] = []
            elif v:
                p["pause_services"] = [x.strip() for x in v.split(",")]
    v = input(f"Blank local GPU output in power profiles? [current {c['blank_internal']}, y/n]: ").strip().lower()
    if v in ("y", "n"):
        c["blank_internal"] = v == "y"
    validate(c)
    if hotkeys:
        other = [p for p in KEYD.parent.glob("*.conf") if p != KEYD]
        if other:
            raise Error("Existing keyd configurations: use --no-hotkeys and merge the generated bindings manually")
        if not success("keyd", "--version"):
            raise Error("Install keyd or use --no-hotkeys")
        with tempfile.TemporaryDirectory() as t:
            test = Path(t) / "prismswitch.conf"
            test.write_text(keyd_config(c))
            run("keyd", "check", test)
        previous = KEYD.read_text() if KEYD.exists() else None
        atomic(KEYD, keyd_config(c))
        KEYD.chmod(0o644)
        try:
            if success("systemctl", "is-active", "--quiet", "keyd.service"):
                run("keyd", "reload")
            else:
                run("systemctl", "enable", "--now", "keyd.service")
        except BaseException:
            if previous is None:
                KEYD.unlink(missing_ok=True)
            else:
                atomic(KEYD, previous)
                KEYD.chmod(0o644)
            if success("systemctl", "is-active", "--quiet", "keyd.service"):
                run("keyd", "reload", check=False)
            raise
    save(CONFIG, c)
    CONFIG.chmod(0o644)
    print("Configuration saved")

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", action="version", version=VERSION)
    sub = parser.add_subparsers(dest="command", required=True)
    for cmd in ("on", "off", "toggle", "status", "kick", "restore", "validate", "keyd-config"):
        sub.add_parser(cmd)
    prof = sub.add_parser("profile")
    prof.add_argument("name")
    conf = sub.add_parser("configure")
    conf.add_argument("--no-hotkeys", action="store_true")
    args = parser.parse_args()
    c = load()
    if args.command == "validate":
        print("Configuration valid")
        return
    if args.command == "keyd-config":
        print(keyd_config(c), end="")
        return
    if os.geteuid() != 0:
        raise Error("Use sudo prismswitch " + args.command)
    with LOCK.open("a") as f:
        fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)
        if args.command == "configure":
            configure(c, not args.no_hotkeys)
            return
        if args.command == "restore":
            restore()
            return
        dev = device(c)
        current = mode_native(dev)
        state = json.loads(STATE.read_text()) if STATE.exists() else None
        if args.command == "status":
            print(f"TDM={current} profile={state['profile'] if state else 'normal'}")
            if state and not state["active"]:
                raise Error("Incomplete power profile; run restore")
            return
        if args.command == "profile":
            if args.name not in c["profiles"]:
                raise Error("Unknown power profile")
            if state and state["profile"] == args.name:
                restore()
                route(dev, "off")
            else:
                restore()
                if current != "on":
                    route(dev, "on")
                apply_profile(c, args.name)
            return
        restore()
        if args.command == "kick":
            route(dev, "off")
            time.sleep(3)
            route(dev, "on")
            if state and state["active"] and state["profile"] in c["profiles"]:
                apply_profile(c, state["profile"])
        else:
            target = {"on": "on", "off": "off", "toggle": "off" if current == "on" else "on"}[args.command]
            route(dev, target)

if __name__ == "__main__":
    def interrupted(signum, frame):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, interrupted)
    try:
        main()
    except (Error, OSError, ValueError, KeyError, TypeError, subprocess.TimeoutExpired, KeyboardInterrupt) as e:
        print(f"PrismSwitch: {e}", file=sys.stderr)
        sys.exit(1)
