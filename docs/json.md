# bm JSON Output Schemas — Version 1

`bm --json <command>` emits machine-readable JSON for scripting and agent
use. The human output is unchanged when the flag is absent. **These shapes
are stable contracts**: any breaking change bumps the version and is
documented here. Adding a new *optional* key is a compatible change; removing
or renaming a key, or changing a type, is breaking.

Applies to: `list`, `current`, `doctor`, `proxy show`, `status`, `size`.
Other commands ignore `--json`. Errors are reported on stderr with a
non-zero exit regardless of the flag.

All timestamps are RFC 3339 strings in UTC. Optional values are `null` or
the key is omitted as noted.

## `bm --json list`

```json
{
  "browser": "chrome",
  "profiles": [
    {
      "name": "work",
      "description": "day job",
      "active": false,
      "proxy": "socks5://10.0.0.1:1080",
      "last_switched_at": null,
      "created_at": "2026-10-27T18:00:00+00:00",
      "updated_at": "2026-10-27T18:00:00+00:00"
    }
  ]
}
```

- `profiles[].proxy` — the egress proxy as `scheme://host:port`, or `null`.
- `profiles[].last_switched_at` — when this profile became active (only
  meaningful together with `"active": true`); `null` otherwise.
- `description` is omitted when empty (profile created without a note).

## `bm --json current`

```json
{
  "browser": "chrome",
  "active": "work",
  "managed": true,
  "proxy": "socks5://10.0.0.1:1080"
}
```

- `active` — the active profile name, `"external (<path>)"` when the
  browser's default data dir is a symlink bm does not manage, or `null`
  when the browser runs on its stock, unmanaged data.
- `managed` — `false` for external/unmanaged activations.
- `proxy` — the active profile's egress proxy, or `null`.

## `bm --json doctor`

```json
{
  "browser": "chrome",
  "issues": [
    {
      "check": "default-symlink",
      "detail": "default-data-dir symlink is dangling: …",
      "auto_fixable": true
    }
  ],
  "issue_count": 1,
  "auto_fixable": 1,
  "applied": 0,
  "remaining": 1
}
```

- `issues` — findings detected at the start of the run.
- `applied` — repairs applied (always `0` without `--fix`).
- `remaining` — findings still open after repairs (a fresh check run with
  `--fix`; identical to `issue_count` without it).

## `bm --json proxy show [name]`

One shape for both forms; passing a name filters to that profile and fails
(non-zero exit) when it does not exist.

```json
{
  "browser": "chrome",
  "profiles": [
    {
      "name": "work",
      "active": true,
      "proxy": "socks5://10.0.0.1:1080",
      "bypass": "localhost,*.lan"
    }
  ]
}
```

- `proxy` — `null` when the profile has no proxy.
- `bypass` — the comma-joined bypass rules, or `null`.

## `bm --json status`

The same report `bm status` renders as a table.

```json
{
  "browser": "chrome",
  "active_profile": "work",
  "profiles": [
    {
      "name": "work",
      "description": "day job",
      "active": true,
      "running": true,
      "running_pid": 48211,
      "proxy": "socks5://10.0.0.1:1080",
      "latest_backup": {
        "path": "/Users/me/.bm/backups/chrome-work-20261027120000.tar.zst",
        "created_at": "2026-10-27T12:00:00+00:00",
        "bytes": 1234567,
        "encrypted": false
      }
    }
  ],
  "doctor": {
    "issues": [{ "check": "permissions", "detail": "…", "auto_fixable": false }],
    "auto_fixable": 0
  }
}
```

- `active_profile` — omitted (`null`) when no profile is active; the
  `external (<path>)` marker for foreign activations.
- `profiles[].running` — a live browser process holds the profile dir
  (Chromium `SingletonLock`); `running_pid` is present only when running.
- `latest_backup` — omitted when the profile has no snapshots; `encrypted`
  marks age-encrypted (`.tar.zst.age`) archives.
- `doctor` — the same findings `bm doctor` reports; `issues` may be empty.
- `description`, `running_pid`, `proxy`, `latest_backup` are omitted when
  empty/absent.

## `bm size --json` (pre-existing shape, unchanged)

A bare array, one entry per profile:

```json
[
  { "name": "work", "total_bytes": 123, "cache_bytes": 100, "core_bytes": 23 }
]
```

## `bm template list --json`

```json
{
  "browser": "chrome",
  "templates": [
    {
      "manifest_version": 1,
      "name": "dev",
      "browser": "chrome",
      "source_profile": "work",
      "description": "Standard development environment",
      "created_at": "2026-10-28T08:30:00Z",
      "bm_version": "0.0.8",
      "components": [
        "Bookmarks",
        "Preferences",
        "Extensions",
        "Local State"
      ]
    }
  ]
}
```

## `bm group list --json`

```json
{
  "groups": [
    {
      "name": "work-suite",
      "description": "Daily corporate workspace",
      "members": [
        {
          "browser": "chrome",
          "profile": "work-1",
          "urls": ["https://jira.example.com"],
          "proxy": null
        }
      ],
      "created_at": "2026-10-28T08:30:00Z",
      "updated_at": "2026-10-28T08:30:00Z"
    }
  ]
}
```

## `bm group status --json`

```json
[
  {
    "name": "work-suite",
    "description": "Daily corporate workspace",
    "member_count": 2,
    "running_count": 1,
    "members": [
      {
        "browser": "chrome",
        "profile": "work-1",
        "running": true,
        "pid": 12345,
        "proxy": "socks5://10.0.0.1:1080",
        "cdp_endpoint": "ws://127.0.0.1:54321/devtools/browser/abc",
        "urls": ["https://jira.example.com"]
      }
    ]
  }
]
```

## `bm lease status --json`

```json
[
  {
    "browser": "chrome",
    "profile": "work-1",
    "holder": "agent-worker-1",
    "pid": 12345,
    "reason": "CDP automation workflow",
    "claimed_at": "2026-10-28T10:00:00Z",
    "heartbeat_at": "2026-10-28T10:02:00Z",
    "expires_at": "2026-10-28T10:05:00Z"
  }
]
```

## Versioning

- Schema version 1: initial documented set (this file), bm 0.0.5+.
- Consumers should tolerate unknown keys (additive changes are compatible).
