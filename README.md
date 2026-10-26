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

### Clone a Profile (Same Machine)
Duplicate an existing profile with its sessions and cookies — ideal for experimenting on a copy. Intentionally local-only: no cross-machine sync:
```bash
bm clone work work-experiment
bm clone work sandbox --no-proxy   # without the source's egress proxy
bm clone work archive --full       # include regenerable caches (slower)
```
* Regenerable caches (HTTP/GPU/shader caches, crash dumps, component stores) are skipped by default — sessions, cookies, history, logins, and extensions come along.
* Cloning refuses to run while the browser is running (same guard as `bm switch`) to avoid copying mid-write databases.

### Switch Active Profile (Conda-like Activation)
Switch the globally active browser profile:
```bash
bm switch work
```
Toggle back to the profile you were on before the last switch (`-` resolves to the previous profile from `~/.bm/state.json`):
```bash
bm switch -
```
*Note: If the browser is currently running, close it first or use `--force` to restart it cleanly:*
```bash
bm switch personal --force
```
`bm list` shows a `SWITCHED` column with how long ago the active profile was activated.

### Interactive Profile Picker
Run bare `bm` — or `bm switch` without arguments — in a terminal to pick a profile interactively: the active one is preselected, descriptions show as hint lines, Enter switches, Esc/Ctrl-C cancels without changes. In scripts and CI (non-TTY), bare `bm` prints help and `bm switch` requires a name.

### Update Notes / Description
```bash
bm describe work "Updated corporate note"
```

### Rename a Profile
Move a profile's directory and metadata to a new name — sessions, cookies, proxy config, and creation time are preserved. If the renamed profile is the globally active one, the default-data-dir symlink is repointed (refuses while the browser is running):
```bash
bm rename work work-2026
bm rename work work-2026 --regen-shortcuts   # also regenerate stale standalone shortcuts
```
Standalone shortcuts referencing the old name are detected and reported; regenerate them with `bm shortcut <new>` or the flag above.

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

### Show Disk Usage (Cache vs Core)
Per-profile byte accounting, split into regenerable cache and valuable core data:
```bash
bm size                 # every profile, sorted by total size
bm size work            # single profile detail
bm size --json          # machine-readable: {name, total_bytes, cache_bytes, core_bytes}
```
Sizes are best-effort snapshots; they are racy while the browser is writing.

### Clean Regenerable Caches
Reclaim disk space by deleting only regenerable state (HTTP/GPU/shader caches, crash dumps, component stores — see the cache manifest). Sessions, cookies, history, and logins are never touched, and nothing outside `~/.bm/profiles` is ever modified:
```bash
bm clean work --dry-run   # list what would be deleted and the reclaimable total
bm clean work             # purge one profile
bm clean --all            # purge every profile of the selected browser
```
Refuses to run while the browser is running (same guard as `bm switch`).

### Health Audit
Audit the bm-managed state for common problems — dangling or external default-data-dir symlinks, registry entries without profile dirs (and vice versa), standalone shortcuts pointing at deleted profiles, macOS managed-policy proxy conflicts with the active profile, and overly permissive `~/.bm` permissions:
```bash
bm doctor          # report only
bm doctor --fix    # apply the safe repairs: remove a dangling symlink, backfill missing metadata
```
Everything marked `report-only` needs manual action; the command prints the suggested fix for each finding.

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

#### Verify Egress (`bm proxy test`)
Confirm a profile's traffic really exits through its proxy — `bm` fetches an IP-echo endpoint through the proxy and once directly, then compares the observed IPs:
```bash
bm proxy test              # every profile with a proxy configured
bm proxy test work         # one profile
bm proxy test work --url https://ifconfig.me --timeout 10
```
* `WARN` means the exit IP equals the direct IP — the proxy is being bypassed.
* Failures print the likely cause: connection refused → the proxy is down; timeout → egress blocked.

> **Positioning:** this is per-profile egress management, not browser fingerprint spoofing. Timezone, language, and device fingerprints remain native. For anti-association multi-account operation on strict platforms, use a dedicated anti-detect browser instead.

### Delete a Profile
Safely delete a profile and its underlying data (active profile is protected from accidental deletion):
```bash
bm delete old-profile
bm delete test-profile --yes   # Skip confirmation prompt
```

### Shell Completions
Generate completions from the live CLI definition (stays in sync with every subcommand and flag):
```bash
# bash
bm completions bash > ~/.local/share/bash-completion/completions/bm

# zsh
bm completions zsh > ~/.zfunc/_bm   # ensure ~/.zfunc is in $fpath before compinit

# fish
bm completions fish > ~/.config/fish/completions/bm.fish
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
