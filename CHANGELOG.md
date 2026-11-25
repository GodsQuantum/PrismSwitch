# Changelog

## 0.2.0 — native Rust controller

- Replaced Python runtime with a native Rust binary and locked Cargo dependencies.
- Retained the original C SMC helper without modifications.
- Added automatic fan safety checks, explicit recovery, signal-aware transactions, native build receipts and configurable keyd application.
- Verified real CPUFreq asynchronous readback on iMac11,1; bounded readback waits and final transaction verification.
- Preserved normal/eco shortcuts, custom profiles, internal DPMS recovery and exact rollback.

## 0.1.0 — preview

- Initial named fork, preserved upstream SMC helper and history.
- Serialized routing controller with native status reads and SMC rebind.
- Reversible named CPU power profiles, optional user-service pause and sleep inhibitor.
- Terminal configuration wizard and optional keyd integration.
- English, French and Simplified Chinese README files, original vector brand kit.
- Automated tests and live read-only check; physical switching of the new controller remains pending.
