# Technical and recovery guide

## Architecture

The native Rust controller uses a locked Cargo dependency graph. The upstream SmcDumpKey.c helper uses x86 I/O ports and is unchanged. PrismSwitch holds an advisory lock, reads MVHR through the bound applesmc driver, briefly unbinds that driver for a routing write, requests MVHR 0/1 and MVMR 2, retries transient failures, and always attempts to rebind. MVHR is the requested routing state, not proof of visible pixels. The shared kernel key selector is restored after status reads. The controller checks automatic SMC fan control and minimum RPM before and after routing, refuses manual fan modes and attempts driver rebind through a recovery guard. SIGINT/SIGTERM/SIGHUP request a safe transaction exit; SIGKILL and power loss cannot be recovered by a process guard. Concurrent SMC utilities must not access the device while switching.

The Rust power controller captures exact original CPU control values and explicitly chosen running user services in a root-owned runtime state file. Failed transitions roll back; unsuccessful rollback retains state for retry. The sleep inhibitor is a transient systemd unit, not a polling daemon. Keyd is optional and uses physical keyboard events without needing a graphical login.

## Configurable profiles

Configuration: /etc/prismswitch/config.json. Run the interactive wizard or edit as root, validate, and regenerate bindings. Names must use lowercase ASCII letters, digits, underscore or hyphen. Shortcut combinations need two or more distinct modifiers (Ctrl, Alt, Shift, Super) and a letter, digit or F1–F12. Duplicate chords, including modifier aliases, are rejected. Pause only user services you actually want stopped; never audio, keyboard handling, networking or safety/thermal services. Do not run another power-management tool that overrides CPU settings while a profile is active.

For example, add a "quiet" profile with cpu_percent 35, disable_boost true, and pause_services [] to profiles, then bind "quiet" to Ctrl+Alt+Q under shortcuts. With an explicit non-root local user, pause_services may include a service such as syncthing.service. Known audio, compositor, D-Bus and keyboard service prefixes cannot be paused. The default eco profile pauses nothing unless you opt in. Enable blank_internal only after verifying the source picture, and only with the supported compositor tools. GPU clock, PCIe power, SATA ALPM and SSD standby are intentionally not changed.

## Recovery

Keep an SSH session ready. If the source has restarted and the panel stays black despite MVHR=1, use the kick command (off, wait 3 s, on). The source may need to re-detect the display. On some desktop login managers, booting headless then hot-plugging an output can leave only a cursor; restart the affected greeter only if no logged-in graphical user would lose work. If it is still black, select the internal route and check the source/cable. Do not repeatedly modify EFI NVRAM or force GPU resets. Audio is independent: select the detected digital sink on the source; an EDID/ELD declaration is not proof of audible sound.

Use sudo prismswitch restore to recover power settings. Do not delete the runtime state manually after a failed rollback. A reboot resets runtime CPU settings, but is not necessary for normal switching. If applesmc fails to rebind, stop switching and inspect the driver and fan behavior from SSH. Do not leave SMC safety control disabled.

## Supported evidence

- Prior, machine-specific implementation on iMac11,1: external DP image physically confirmed by the owner, EDID 256 bytes, 2560×1440 at 59.95 Hz and stable fan control. CPU cap and rollback tested.
- New portable controller: Rust unit tests with fake hardware, failure injection, fan checks, locking and selector restoration; upstream C helper built; shell syntax checks. Live evidence is recorded separately below.
- Pending: new controller installation and full bidirectional physical switching, profile enter/exit on actual hardware, physical hotkeys and audible audio. No claims of watt savings.
- Mid-2010 technique: original floe/smc_util README, not an independent certification of this release.

See upstream README and source history. No Thunderbolt TDM implementation.
