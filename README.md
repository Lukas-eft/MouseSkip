<div align="center">

<img src="assets/mouseskip-banner.png" alt="MouseSkip Banner" width="100%" />

<br />
<br />

[![Rust 2021](https://img.shields.io/badge/Rust-2021_Edition-EA580C?style=for-the-badge&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Windows 10 / 11](https://img.shields.io/badge/Platform-Windows_10_%2F_11-0078D4?style=for-the-badge&logo=windows11&logoColor=white)](https://www.microsoft.com/windows)
[![License: MIT](https://img.shields.io/badge/License-MIT-60A5FA?style=for-the-badge)](#license)

**A lightweight Windows Raw Input (`WM_INPUT`) mouse skip, polling dropout, and sensor spinout monitor.**

MouseSkip captures hardware-level mouse deltas directly via Win32 `WM_INPUT` (`RIDEV_INPUTSINK`) and displays live polling rate estimates, dropout/spinout timelines, and configurable thresholds in a native Windows 11 Mica WebView2 dashboard with System Tray background support.

</div>

---

## 🚀 Setup Guide

### Prerequisites
- **Windows 10 / 11**
- **Rust & Cargo** (`edition = "2021"`, via [rustup](https://rustup.rs/))
- **Microsoft Edge WebView2 Runtime** (pre-installed on Windows 11 and modern Windows 10)

### Build & Run
```powershell
git clone https://github.com/Lukas-eft/MouseSkip.git
cd MouseSkip
cargo build --release
cargo run --release
```

### Optional Verification
```powershell
# Run unit tests
cargo test

# Verify Win32 Raw Input sink registration
cargo run -- --smoke-test
```

---

## ⚠️ Disclaimer

**MouseSkip** is an independent, non-commercial personal hobby project built to inspect and troubleshoot mouse sensor and polling behavior on Windows.

- **Heuristic Measurements:** Polling rate estimates, dropout flags, and spinout detections are heuristic measurements derived from user-mode `WM_INPUT` message timing and may be affected by Windows scheduler preemption, USB hub power management, or compositor message coalescing. Results are provided as-is without any warranty.
- **No Affiliation:** Not affiliated with, endorsed by, or connected to any peripheral manufacturer or software vendor. If you have any issues or removal requests, please open an [Issue](https://github.com/Lukas-eft/MouseSkip/issues).

---

## License

This project is released under the [MIT License](LICENSE).
