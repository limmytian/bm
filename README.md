<p align="center">
  <img src="assets/bm-light.png" width="220" alt="bm Logo" />
</p>

<h1 align="center">bm</h1>

<p align="center">
  <strong>Browser Profile & Session Sandbox Manager for Chrome, Edge, Brave & Chromium</strong>
</p>

<p align="center">
  <a href="https://www.apache.org/licenses/LICENSE-2.0"><img src="https://img.shields.io/badge/License-Apache_2.0-blue.svg" alt="License: Apache 2.0"></a>
  <a href="https://github.com/limmytian/homebrew-tap"><img src="https://img.shields.io/badge/Homebrew-limmytian%2Ftap-orange.svg" alt="Homebrew Tap"></a>
</p>

`bm` is an ultra-fast, lightweight CLI written in Rust to manage, isolate, switch, and launch browser user profiles (sessions, cookies, local storage, history) in a **conda-like workflow**.

**Google Chrome**, **Microsoft Edge**, **Brave**, and **Chromium** are natively supported.

---

## 🌟 Key Features

* **Centralized Profile Storage**: All profiles and session databases are organized neatly under `~/.bm/profiles/<browser>/<name>`.
* **Symlink-Driven Profile Switching (Conda-like)**:
  * Running `bm switch <name>` atomically swaps the browser's default user data path with a symlink pointing to your chosen profile.
  * Any subsequent launch—whether from macOS Dock, Spotlight, Launchpad, or external URL clicks—will automatically use the activated profile with zero command-line flags.
* **Safe Automatic First-Run Migration**: When switching for the first time, your pre-existing browser data is automatically safely backed up as profile `original`.
* **Running Process Guard**: Warns or terminates running browser instances to prevent SQLite database locks when switching.
* **Standalone Desktop Shortcuts**: `bm shortcut <name>` generates an independent macOS `.app` bundle or Linux launcher script, allowing you to run multiple accounts side-by-side without mutating the globally active symlink.
* **Multi-Browser Support**: Seamlessly manage Chrome, Edge, Brave, and Chromium environments with `--browser chrome|edge|brave|chromium`. Firefox and Arc are intentionally out of scope — their profile models differ from the Chromium user-data-dir layout bm is built around.
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
bm create clean-dev --template dev --note "Clean sandbox seeded from 'dev' template"
```

### Profile Templates (`bm template`)
Extract extensions, bookmarks, and preferences from an existing profile into a reusable template bundle under `~/.bm/templates/<name>`, with a strict physical-level guarantee of zero cookies, sessions, logins, history, or website local storage:
```bash
bm template save dev work --note "Developer base template"
bm template list
bm template list --json
bm template delete dev
```
* **Strict zero-session guarantee**: Cookies, session tokens, saved credentials, history, and website HTML5 storage (IndexedDB, WebSQL, localStorage) are physically stripped and verified with recursive post-extraction audits.
* **Instant initialization**: Seed fresh profiles from saved templates using `bm create <name> --template <template>`:
```bash
bm create fresh-worker --template dev
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
bm open work personal guest          # multi-account: three profiles side by side
bm open work personal https://gmail.com   # same URLs for every launched profile
```
* The CLI returns immediately; each profile's egress proxy is applied automatically.
* **Multi-profile grammar**: the first token is always a profile name; every token after it that is a *valid profile name* launches as another profile, and anything else (contains `.`, `:`, `/`, …) opens as a URL in all launched profiles. This keeps `bm open <name> [urls...]` fully back-compatible. Per-profile launch results are reported; a missing profile is reported and the run exits non-zero.
* Running browser instances are fine: a distinct profile directory runs side by side. Re-running `bm open <name> <url>` hands the URLs to that profile's running instance.

#### Ephemeral Sandboxes (`bm open --temp`)
Spawn a disposable, completely isolated browser sandbox under `~/.bm/temp/<id>` that is automatically purged upon browser process exit:
```bash
bm open --temp
bm open --temp https://example.com
bm open --temp --template dev https://github.com
bm open --temp --cdp
```
* Spawns the browser detached and returns immediately.
* A detached background supervisor watches the browser process and deletes `~/.bm/temp/<id>` the moment the browser closes.
* Optionally pre-seeded from a template bundle (`--template <tpl>`).
* Supports `--cdp` for automated disposable browser agent runs.

### Multi-Profile Workspaces & Session Groups (`bm group`)
Manage and orchestrate clusters of browser profiles across browsers (Chrome, Edge, Brave, Chromium) as coherent workspaces:
```bash
# Manage groups and memberships
bm group create work-suite --note "Daily engineering workspace"
bm group add work-suite work-main --url https://mail.google.com
bm group add work-suite ops-edge --browser edge --proxy socks5://127.0.0.1:1080 --url https://grafana.internal
bm group list
bm group show work-suite

# Orchestrate concurrent lifecycles
bm group open work-suite          # launch all member profiles side by side in parallel
bm group status work-suite        # inspect live running instances, PIDs, and CDP endpoints
bm group close work-suite         # graceful multi-process teardown (via CDP / SIGTERM)
bm group close work-suite --force # force kill if processes refuse to exit
```
* **Concurrent parallel launch**: `bm group open` launches every member profile detached with its own isolated directory, member-level proxy overrides, and designated startup URLs.
* **Graceful multi-process shutdown**: `bm group close` coordinates targeted teardown across browsers, trying CDP `Browser.close` first, then SIGTERM, and only SIGKILL upon `--force`.
* **Scriptable inspection**: `bm group list --json` and `bm group status --json` output machine-readable statuses for agent orchestration.

### Agent Lease Locks (`bm lease`)
Prevent accidental human interventions or conflicting scripts while an autonomous agent controls a profile:
```bash
# Acquire a lease lock (default 300s TTL, auto-records caller PID)
bm lease claim work-1 --holder "agent-scraper-01" --ttl 300 --reason "Batch CDP workflow"

# Extend lease expiration during long operations
bm lease heartbeat work-1 --ttl 300

# Inspect active leases
bm lease status
bm lease status --json

# Destructive operations (switch, clean, rename, delete) are strictly rejected while locked
bm switch work-1        # Error: Profile 'work-1' is locked by agent lease (holder: 'agent-scraper-01')
bm switch work-1 --force # Override lease lock if authorized

# Release lease when work is done
bm lease release work-1 --holder "agent-scraper-01"
```
* **Mutual exclusion**: Guard against human `bm switch`, `bm clean`, `bm rename`, and `bm delete` during CDP sessions.
* **Dead PID detection**: Stale leases whose holding process has exited are automatically considered inactive.
* **Persistent & Private**: Stored under `~/.bm/leases.json` with strict 0600 file permissions.

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

### Backup & Restore (Local Snapshots)
Snapshot a profile to a timestamped `tar.zst` archive under `~/.bm/backups/` — sessions, cookies, logins, and extensions in one file. Regenerable caches are excluded (`--full` includes them); an embedded manifest carries the description and egress proxy so restores re-register them automatically:
```bash
bm backup work
bm backup work --keep 5        # prune older snapshots beyond the 5 newest
bm backup --all                # snapshot every profile of the selected browser
bm restore ~/.bm/backups/chrome-work-20261027120000.tar.zst
bm restore <archive> --name work-rollback   # restore under a different name
bm restore <archive> --force                # overwrite an existing profile
```
* `bm backup` and `bm restore` refuse to run while the browser is running (same guard as `bm switch`); `bm backup --all` instead **skips and reports** profiles held by a running browser (detected via the Chromium `SingletonLock` pid) and continues with the rest — `--keep` applies per profile.
* `bm delete` now suggests taking a backup first.
* Local-machine only: archives are ordinary files you can copy elsewhere, but bm does not sync them.

#### Verify & Encrypt Backups
```bash
bm backup --verify ~/.bm/backups/chrome-work-20261027120000.tar.zst
bm backup work --encrypt                    # encrypted at rest (age format, tar → zstd → age)
BM_BACKUP_PASSPHRASE=... bm backup --all --encrypt   # non-interactive runs
```
`--verify` validates an archive **without extracting it**: the zstd frame must decode in full, every tar entry must be readable, and the embedded manifest (profile, browser, creating bm version, proxy) must parse. Encrypted archives are decrypted in-stream during verification.

**Encryption threat model, stated plainly:** archives contain your full login state, so `--encrypt` seals them with age v1 (scrypt passphrase recipient). The passphrase comes from `BM_BACKUP_PASSPHRASE` or a hidden prompt — it is **never accepted on the command line**. bm does **no key management**: the passphrase is the only key, a lost passphrase means a lost backup, and only the passphrase holder can restore. `bm restore` and `bm backup --verify` detect encrypted archives (`.tar.zst.age`) automatically and prompt/decrypt on the fly; plain and encrypted snapshots share the same `--keep` retention pool.

#### Scheduled Auto-Backup
Install a daily `bm backup --all` via launchd (macOS) or a systemd user timer (Linux):
```bash
bm backup --schedule install --keep 7    # daily at 03:00, per selected --browser
bm backup --schedule status
bm backup --schedule uninstall
```
The schedule refuses to install twice; logs go to `~/.bm/backup.log` (launchd) or `journalctl --user -u bm-backup-<browser>` (systemd). Test the unit manually with `bm backup --all --keep 7` — that is exactly the command the timer runs.

### CDP Access for Agents (`bm open --cdp`)
Hand agent tooling (Playwright, Puppeteer, browser-use, chrome-remote-interface) a browser with **real sessions and real egress identity** instead of a fresh incognito instance — bm profiles are the identity layer:
```bash
bm open agent-work --cdp
# Warning: --cdp hands FULL control of this browser (sessions, cookies, everything)
# to anyone who can reach the endpoint — loopback only.
# Success: Launched Google Chrome profile 'agent-work' — global default profile unchanged.
# CDP endpoint: ws://127.0.0.1:51234/devtools/browser/8f2a...
```
* **Strictly opt-in, every time**: CDP is enabled only when the exact launch carries `--cdp`. Shortcuts, managed policy, and every other bm path never add debugging flags.
* **No fixed ports, ever**: launches use `--remote-debugging-port=0` (OS-assigned) and bm discovers the real port from `DevToolsActivePort` inside the profile dir — collision-free by construction.
* **Loopback only**: launches pin `--remote-debugging-address=127.0.0.1`. Endpoints are persisted in `~/.bm/cdp.json` with mode 0600.
* **Singleton-safe**: re-running `bm open <name> --cdp` on a running profile returns the live endpoint instead of spawning a duplicate; a profile already running *without* `--cdp` is reported (a re-launch cannot enable debugging on a running instance).
* Chrome 136+ restricts debugging on the *default* user-data-dir — bm profiles are custom dirs, so this is compliant.

Inspect and close:
```bash
bm cdp status                 # live endpoints (dead entries purged)
bm cdp close agent-work       # graceful shutdown via the CDP Browser.close method
```

#### Connecting from Agent Tooling
```python
# Playwright (Python) — full browser, real logins, profile's egress proxy
browser = playwright.chromium.connect_over_cdp("ws://127.0.0.1:51234/devtools/browser/…")
```
```javascript
// Puppeteer
const browser = await puppeteer.connect({ browserWSEndpoint: "ws://127.0.0.1:51234/devtools/browser/…" });
```
```javascript
// chrome-remote-interface
const CDP = require('chrome-remote-interface');
const client = await CDP({ webSocketDebuggerUrl: "ws://127.0.0.1:51234/devtools/browser/…" });
```
**Use a dedicated bm profile for agent work** (e.g. `bm create agent-work`). The agent sees and can do everything every session in that profile can — keep your personal logins out of it.

### Status At A Glance
One read-only command that answers "what is bm doing right now?" for the selected browser — no browser launch, fast enough to alias:
```bash
bm status
```
Shows the active profile (including external/unmanaged activations), which profiles have a live browser process and its pid (via the Chromium `SingletonLock`), each profile's egress proxy, the newest backup per profile (timestamp, size, `age` marker when encrypted), and any open `bm doctor` findings with severity.

### Health Audit & Browser Discovery
Audit the browser executable resolution and bm-managed state for common problems — binary resolution sources, dangling or external default-data-dir symlinks, registry entries without profile dirs (and vice versa), standalone shortcuts pointing at deleted profiles, macOS managed-policy proxy conflicts with the active profile, and overly permissive `~/.bm` permissions:
```bash
bm doctor          # report only (includes browser binary resolution status)
bm doctor --fix    # apply the safe repairs: remove a dangling symlink, backfill missing metadata
```
Everything marked `report-only` needs manual action; the command prints the suggested fix for each finding.

#### Browser Auto-Discovery & Custom Overrides
`bm` automatically discovers installed browsers across standard directories and dynamic search indices:
* **macOS**: Probes `/Applications`, `~/Applications`, and queries macOS Spotlight (`mdfind`) by Bundle Identifier (`com.google.Chrome`, `com.microsoft.edgemac`, `com.brave.Browser`, `org.chromium.Chromium`).
* **Linux**: Searches `$PATH`, `/snap/bin`, `/usr/bin`, and `/usr/local/bin`.

To target custom Chromium engines, Canary/Dev/Beta channels, or portable builds, set per-browser environment variables (or the global fallback):
```bash
# Per-browser overrides (take precedence)
export BM_CHROME_PATH="/opt/custom-chrome/Google Chrome.app"
export BM_EDGE_PATH="/usr/local/bin/msedge-dev"
export BM_BRAVE_PATH="$HOME/Applications/Brave Browser Nightly.app"
export BM_CHROMIUM_PATH="/snap/bin/chromium"

# Global fallback (used when no browser-specific variable is set)
export BM_BROWSER_PATH="/path/to/custom/browser"
```

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

### Built-in MCP Server (`bm mcp`)
`bm mcp` runs a Model Context Protocol server over stdio (v1 transport), giving agent hosts schema-driven access to bm without shell access. It is a thin wrapper over the same modules the CLI uses — the running-browser and active-profile guards apply unchanged — and returns the same JSON shapes documented in docs/json.md.

Tools: `list_profiles`, `current`, `status`, `open_profile` (name, browser, urls, `cdp` — CDP stays strictly opt-in per call), `switch` (force), `proxy_get`, `proxy_set`, `proxy_clear`, `backup`. No destructive tools (delete is deliberately not exposed).

Register it in your MCP host config:
```json
{
  "mcpServers": {
    "bm": { "command": "bm", "args": ["mcp"] }
  }
}
```
This snippet works for Claude Desktop (`claude_desktop_config.json`), Cursor (`mcp.json`), and ZCode. For Task Weaver: `tw mcp register-local bm -- bm mcp`. stdout carries only JSON-RPC frames; warnings go to stderr.

### Scriptable JSON Output
`bm --json` gives every read-oriented command a stable, documented JSON form (human output is untouched): `list`, `current`, `doctor`, `proxy show`, `status`, and `size`. The shapes are pinned by golden tests and versioned in [docs/json.md](docs/json.md) — external tooling and `bm mcp` (v1) rely on them verbatim:
```bash
bm --json status | jq '.profiles[] | select(.running) | .name'
bm --json list --browser edge | jq '.profiles[].name'
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

### Man Page
`bm(1)` is generated from the same CLI definition (clap_mangen) and ships in every release archive:
```bash
bm manpage | sudo tee /usr/local/share/man/man1/bm.1 >/dev/null   # then: man bm
```
Release tarballs contain `bm.1`; copy it into any directory on your `manpath` (e.g. `/usr/local/share/man/man1/` or `~/.local/share/man/man1/`).

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
