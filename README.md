<p align="center">
  <img src="assets/bm-light.png" width="220" alt="bm Logo" />
</p>

<h1 align="center">bm</h1>

<p align="center">
  <strong>Browser Profile & Session Sandbox Manager for Chrome & Edge</strong>
</p>

<p align="center">
  <a href="https://www.apache.org/licenses/LICENSE-2.0"><img src="https://img.shields.io/badge/License-Apache_2.0-blue.svg" alt="License: Apache 2.0"></a>
  <a href="https://github.com/limmytian/homebrew-tap"><img src="https://img.shields.io/badge/Homebrew-limmytian%2Ftap-orange.svg" alt="Homebrew Tap"></a>
</p>

`bm` is an ultra-fast, lightweight CLI written in Rust to manage, isolate, switch, and launch browser user profiles (sessions, cookies, local storage, history) in a **conda-like workflow**.

Both **Google Chrome** and **Microsoft Edge** are natively supported.

---

## 🌟 Key Features

* **Centralized Profile Storage**: All profiles and session databases are organized neatly under `~/.bm/profiles/<browser>/<name>`.
* **Symlink-Driven Profile Switching (Conda-like)**:
  * Running `bm switch <name>` atomically swaps the browser's default user data path with a symlink pointing to your chosen profile.
  * Any subsequent launch—whether from macOS Dock, Spotlight, Launchpad, or external URL clicks—will automatically use the activated profile with zero command-line flags.
* **Safe Automatic First-Run Migration**: When switching for the first time, your pre-existing browser data is automatically safely backed up as profile `original`.
* **Running Process Guard**: Warns or terminates running browser instances to prevent SQLite database locks when switching.
* **Standalone Desktop Shortcuts**: `bm shortcut <name>` generates an independent macOS `.app` bundle or Linux launcher script, allowing you to run multiple accounts side-by-side without mutating the globally active symlink.
* **Multi-Browser Support**: Seamlessly manage Google Chrome and Microsoft Edge environments with `--browser chrome` or `--browser edge`.
* **Per-Profile Egress Proxy**: Route each environment through its own SOCKS5/HTTP(S) proxy with WebRTC leak protection and SOCKS5 DNS pinning. Honored by standalone shortcuts out of the box and by globally activated launches via Chrome/Edge managed policy (macOS).

---

## 🚀 Quick Start

### 1. Installation

#### Via Homebrew (Recommended on macOS/Linux)

```bash
brew install limmytian/tap/bm
```

#### Via Cargo (From Source)

```bash
git clone https://github.com/limmytian/bm.git
cd bm
cargo install --path .
```

Verify installation:
```bash
bm --version
bm --help
```

---

## 📖 Command Reference

### List Profiles
List all managed profiles with their active indicator (`*`), description, and creation timestamp:
```bash
bm list
bm list --browser edge
```

### Show Currently Active Profile
```bash
bm current
bm current --browser edge
```

### Create a New Profile
Create a clean, isolated profile with an optional note:
```bash
bm create work --note "Primary corporate accounts (Google, Slack, Jira)"
bm create personal --note "Personal entertainment and social media"
bm create staging --browser edge --note "Edge testing sandbox"
```

### Switch Active Profile (Conda-like Activation)
Switch the globally active browser profile:
```bash
bm switch work
```
*Note: If the browser is currently running, close it first or use `--force` to restart it cleanly:*
```bash
bm switch personal --force
```

### Update Notes / Description
```bash
bm describe work "Updated corporate note"
```

### Generate Standalone Desktop Shortcut
Create a standalone application launcher (e.g., `Chrome (work).app` on macOS) that can be pinned to your Dock:
```bash
bm shortcut work
bm shortcut staging --browser edge
```

### Launch a Profile Ad-Hoc (No Switching)
Launch the browser directly against a profile with its proxy flags, without touching the globally active profile and without generating any launcher artifact — the quick, no-ceremony counterpart to `bm shortcut`:
```bash
bm open work
bm open personal --browser edge https://github.com https://news.ycombinator.com
```
* The CLI returns immediately; the profile's egress proxy is applied automatically.
* Running browser instances are fine: a distinct profile directory runs side by side. Re-running `bm open <name> <url>` hands the URLs to that profile's running instance.

### Manage Per-Profile Proxy
Each profile can route traffic through its own egress proxy (SOCKS5, HTTP, or HTTPS):
```bash
bm proxy set work --server socks5://127.0.0.1:1080
bm proxy set work --server http://proxy.corp.example:8080 --bypass "localhost,*.lan"
bm proxy show            # one profile or all
bm proxy clear work
```
* Standalone shortcuts inject the proxy automatically — regenerate them with `bm shortcut <name>` after changing a profile's proxy.
* For the **globally active** profile, `bm` applies the proxy through Chrome/Edge managed policy on macOS (Dock, Spotlight, and link-click launches honor it too); `bm switch` and `bm proxy set/clear` keep it in sync. On Linux a user-level `.desktop` override carries the flags for desktop launches.
* SOCKS5 proxies additionally pin DNS resolution to the proxy, and non-proxied UDP (WebRTC) is disabled to prevent real-IP leaks.

> **Positioning:** this is per-profile egress management, not browser fingerprint spoofing. Timezone, language, and device fingerprints remain native. For anti-association multi-account operation on strict platforms, use a dedicated anti-detect browser instead.

### Delete a Profile
Safely delete a profile and its underlying data (active profile is protected from accidental deletion):
```bash
bm delete old-profile
bm delete test-profile --yes   # Skip confirmation prompt
```

---

## 📂 Architecture & Directory Layout

```
~/.bm/
├── metadata.json                 # JSON registry storing profiles metadata & notes
├── profiles/
│   ├── chrome/
│   │   ├── original/             # Automatically preserved original data
│   │   ├── work/                 # Isolated Chrome environment (Default/Cookies, History...)
│   │   └── personal/
│   └── edge/
│       ├── test/                 # Isolated Edge environment
│       └── ...
└── shortcuts/                    # Standalone launchers
```

### Default Data Path Symlink Resolution
* **Google Chrome (macOS)**:
  `~/Library/Application Support/Google/Chrome` $\to$ `~/.bm/profiles/chrome/<active-profile>`
* **Microsoft Edge (macOS)**:
  `~/Library/Application Support/Microsoft Edge` $\to$ `~/.bm/profiles/edge/<active-profile>`

---

## 🛠️ Testing

Run the automated test suite:
```bash
cargo test
```

---

## 📄 License

This project is licensed under the [Apache License 2.0](LICENSE).
