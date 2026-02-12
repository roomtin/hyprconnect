# Hyprconnect

Hyprconnect is a Rust companion for KDE Connect on compositor-first desktops (Hyprland/Wayland) with a Waybar-friendly output path.

It consists of:

- `hyprconnectd`: background daemon that polls KDE Connect state and exposes a local Unix socket API.
- `hyprconnectctl`: CLI for status, pairing, sharing, diagnostics, and Waybar JSON output.

The current implementation uses `kdeconnect-cli` as the action backend and reads battery/connectivity telemetry from KDE Connect D-Bus properties.

## What It Solves

- Desktop environments like GNOME have first-class KDE Connect integrations (for example GSConnect), but Hyprland/Waybar users typically need to assemble equivalent behavior manually.
- Hyprconnect provides a focused integration layer with:
  - device reachability + pairing state
  - battery + cellular signal reporting for Waybar
  - file/URL/clipboard sharing from desktop to phone
  - pair/unpair/ping CLI workflows
  - a lightweight diagnostics command (`doctor`)

## Current Feature Set

- Device cache with reachable/offline + paired/unpaired state.
- Battery charge + charging status via KDE Connect D-Bus battery plugin.
- Cellular signal percentage + network type via KDE Connect connectivity report plugin (when available).
- Actions:
  - share file
  - share URL
  - share clipboard
  - ping
  - pair
  - unpair
  - find my phone (ring)
  - refresh discovery
  - mount phone filesystem
  - open mountpoint in file manager
  - toggle mount (unmount when mounted, otherwise mount+open)
  - phone media controls (playback, seek, player selection, volume 0-100)
- Waybar JSON payload generation (`hyprconnectctl waybar-json`).
- Connection-state desktop notifications (displayed by your notification daemon, e.g. `swaync`).
- Event-driven daemon refresh via KDE Connect D-Bus signals, with fallback polling.

## Architecture

- `hyprconnectd`
  - reads `~/.config/hyprconnect/config.toml`
  - refreshes state immediately on KDE Connect signals
  - uses `poll_interval_seconds` as fallback sync interval
  - reads D-Bus properties for battery/connectivity
  - serves IPC over `${XDG_RUNTIME_DIR}/hyprconnect.sock` (fallback `/tmp/hyprconnect.sock`)
- `hyprconnectctl`
  - sends JSON requests to daemon socket
  - prints human output or JSON output depending on command/flags

## Repository Layout

- `crates/hyprconnect-core`: shared config/state/IPC types.
- `crates/hyprconnectd`: daemon executable.
- `crates/hyprconnectctl`: user-facing CLI.
- `examples/config.toml`: sample config.
- `examples/systemd/hyprconnectd.service`: reference user service unit.

## Requirements

Required runtime dependencies:

- `kdeconnect` package (must provide `kdeconnect-cli` and `kdeconnectd`)
- `wl-clipboard` (`wl-paste`)
- `busctl` (from `systemd`; generally present on modern Linux)

Recommended environment assumptions:

- Android KDE Connect app installed and permissions granted.
- Desktop and phone on same local network.
- Notification daemon running (for connection notifications).

## Build

```bash
cargo build --release
```

Binaries:

- `target/release/hyprconnectd`
- `target/release/hyprconnectctl`

## Configuration

Configuration path:

- `~/.config/hyprconnect/config.toml`

Example:

```toml
default_device = ""
poll_interval_seconds = 10
battery_warn_percent = 30
battery_crit_percent = 15
notifications_enabled = true
```

Field reference:

- `default_device`
  - device id string used as first choice for actions.
  - if empty/unset, daemon selects first paired+reachable device.
- `poll_interval_seconds`
  - daemon refresh period.
  - lower values improve responsiveness but increase command churn.
- `battery_warn_percent`, `battery_crit_percent`
  - currently reserved for future dynamic thresholding in payload generation.
  - Waybar class thresholds currently follow module logic in `hyprconnectctl`.
- `notifications_enabled`
  - when true, daemon emits local notifications on connect/disconnect transitions.

## Running Hyprconnect

### Manual start

Terminal 1:

```bash
./target/release/hyprconnectd
```

Terminal 2:

```bash
./target/release/hyprconnectctl doctor
./target/release/hyprconnectctl status
```

### systemd user service

Install unit to:

- `~/.config/systemd/user/hyprconnectd.service`

Then:

```bash
systemctl --user daemon-reload
systemctl --user enable --now hyprconnectd.service
systemctl --user status hyprconnectd.service
```

## Pairing Workflow

1. Ensure phone is visible:

```bash
hyprconnectctl list-available
```

2. Send pair request:

```bash
hyprconnectctl pair --device <device-id>
```

3. Accept on phone if prompted.

4. Verify:

```bash
hyprconnectctl status
hyprconnectctl devices --json
```

To remove trust:

```bash
hyprconnectctl unpair --device <device-id>
```

## Command Reference

Use `hyprconnectctl --help` and `hyprconnectctl <command> --help` for full command-level help.

- `hyprconnectctl status`
  - human summary of all cached devices.
- `hyprconnectctl devices [--json]`
  - list all devices known by daemon cache.
- `hyprconnectctl list-available [--json]`
  - list only reachable devices.
- `hyprconnectctl pair --device <id>`
  - request pairing to device id.
- `hyprconnectctl unpair --device <id>`
  - remove pairing with device id.
- `hyprconnectctl share-file <path> [--device <id>]`
  - share a local file.
- `hyprconnectctl share-url <url> [--device <id>]`
  - share a URL.
- `hyprconnectctl share-clipboard [--device <id>]`
  - share clipboard contents.
- `hyprconnectctl ping [--device <id>] [--message <text>]`
  - send ping notification.
- `hyprconnectctl waybar-json`
  - emit JSON object for Waybar custom module (`text`, `tooltip`, `class`).
- `hyprconnectctl doctor`
  - run prerequisite checks (binary presence + socket health).
- `hyprconnectctl refresh`
  - ask KDE Connect to rediscover devices.
- `hyprconnectctl find [--device <id>]`
  - ring target phone via find-my-phone plugin.
- `hyprconnectctl mount [--device <id>]`
  - mount phone filesystem via KDE Connect SFTP plugin.
- `hyprconnectctl open-mount [--device <id>]`
  - mount then open internal storage path (`<mountpoint>/storage/emulated/0`) with `xdg-open`.
- `hyprconnectctl toggle-mount [--device <id>]`
  - unmount if mounted, otherwise mount and open internal storage.
- `hyprconnectctl media --device <id> status`
  - show active phone player status.
- `hyprconnectctl media --device <id> play-pause|next|previous|stop`
  - control phone media playback.
- `hyprconnectctl media --device <id> seek --ms <delta>`
  - seek phone media by milliseconds.
- `hyprconnectctl media --device <id> volume --set <0-100>`
  - set phone media volume.
- `hyprconnectctl media --device <id> player-list`
  - list available phone media players.
- `hyprconnectctl media --device <id> player-set --name <player>`
  - set active phone media player.
- `hyprconnectctl completions --shell <shell>`
  - print completion script to stdout for `bash`, `zsh`, `fish`, `elvish`, or `powershell`.

## Shell Completions

Generate completion scripts directly from the CLI.

Examples:

```bash
# Bash (user-local)
mkdir -p ~/.local/share/bash-completion/completions
hyprconnectctl completions --shell bash > ~/.local/share/bash-completion/completions/hyprconnectctl

# Zsh (user-local)
mkdir -p ~/.zfunc
hyprconnectctl completions --shell zsh > ~/.zfunc/_hyprconnectctl

# Fish (user-local)
mkdir -p ~/.config/fish/completions
hyprconnectctl completions --shell fish > ~/.config/fish/completions/hyprconnectctl.fish
```

After installation, restart your shell or source your shell config.

## Waybar Integration

The companion script used in your dotfiles:

- `/home/banana/dotfiles/.config/waybar/scripts/hyprconnect-status.sh`

Behavior:

- resolves `hyprconnectctl` in this order:
  - `~/Projects/hyprconnect/target/release/hyprconnectctl`
  - `~/Projects/hyprconnect/target/debug/hyprconnectctl`
  - `PATH`
- default mode runs `hyprconnectctl waybar-json`.
- `share-clipboard` mode triggers clipboard send.

Expected Waybar module fields:

- `text`: compact status line with cellular icon ramp, phone icon, battery %, and optional charging bolt.
- `tooltip`: multiline details (device, battery, status, pairing, signal, network).
- `class`: `ok`, `warn`, `crit`, or `disconnected`.

## SwayNC Media Widget

For persistent top-of-panel media controls, enable the SwayNC `mpris` widget above notifications.
This surfaces both local media and phone media exposed by KDE Connect MPRIS bridges.

## Troubleshooting

- `doctor` says `kdeconnect-cli: missing/fail`
  - install KDE Connect package and verify `kdeconnect-cli --list-devices` works.
- `doctor` says `hyprconnectd socket: missing/fail`
  - start daemon or check user service status.
- Waybar shows disconnected fallback icon
  - run `hyprconnectctl waybar-json` manually.
  - if that fails, resolve daemon/CLI first.
- Device appears connected but no battery
  - ensure phone has granted battery/connectivity permissions in KDE Connect app.
  - verify D-Bus battery path exists under `org.kde.kdeconnect` device tree.
- Slow updates
  - ensure signal listener is healthy (`journalctl --user -u hyprconnectd -f`).
  - keep Waybar `interval = 1`; fallback polling can stay at `10`.

## Known Limitations

- Remote input is intentionally deferred.
- Not all phones expose complete connectivity metadata.
- Threshold config values are not yet dynamically wired into Waybar class mapping.

## Security Notes

- Actions are limited to locally authenticated user session.
- Pairing trust remains controlled by KDE Connect itself.
- No cloud relay is used by Hyprconnect; traffic follows KDE Connect behavior.
