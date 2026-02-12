use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use futures_util::StreamExt;
use hyprconnect_core::{
    runtime_socket_path, Config, DaemonState, DeviceState, IpcRequest, IpcResponse, MediaAction,
};
use notify_rust::Notification;
use regex::Regex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::process::Command;
use tokio::sync::RwLock;
use tokio::time::{sleep, Duration};
use zbus::message::Type as MessageType;

#[derive(Clone)]
struct Shared {
    state: Arc<RwLock<DaemonState>>,
    config: Config,
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::load().unwrap_or_default();
    let shared = Shared {
        state: Arc::new(RwLock::new(DaemonState::default())),
        config: config.clone(),
    };

    let socket = runtime_socket_path()?;
    if socket.exists() {
        let _ = std::fs::remove_file(&socket);
    }
    let listener = UnixListener::bind(&socket)
        .with_context(|| format!("failed to bind socket: {}", socket.display()))?;

    let bg = shared.clone();
    tokio::spawn(async move {
        loop {
            if let Err(err) = refresh_state(&bg).await {
                eprintln!("refresh failed: {err:#}");
            }
            sleep(Duration::from_secs(bg.config.poll_interval_seconds.max(10))).await;
        }
    });

    let events = shared.clone();
    tokio::spawn(async move {
        loop {
            if let Err(err) = listen_for_kdeconnect_events(events.clone()).await {
                eprintln!("event listener failed: {err:#}");
                sleep(Duration::from_secs(2)).await;
            }
        }
    });

    if let Err(err) = refresh_state(&shared).await {
        eprintln!("initial refresh failed: {err:#}");
    }

    loop {
        let (stream, _) = listener.accept().await?;
        let s = shared.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_stream(stream, s).await {
                eprintln!("ipc request failed: {err:#}");
            }
        });
    }
}

async fn handle_stream(mut stream: UnixStream, shared: Shared) -> Result<()> {
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await?;
    let req: IpcRequest = serde_json::from_slice(&buf).context("invalid IPC request JSON")?;

    let resp = match req {
        IpcRequest::GetState => IpcResponse {
            ok: true,
            message: None,
            state: Some(shared.state.read().await.clone()),
        },
        IpcRequest::ShareFile { path, device } => {
            let result = share_path(&shared, &path, device).await;
            into_response(result)
        }
        IpcRequest::ShareUrl { url, device } => {
            let result = share_path(&shared, &url, device).await;
            into_response(result)
        }
        IpcRequest::ShareClipboard { device } => {
            let clip = read_clipboard().await?;
            let result = share_path(&shared, &clip, device).await;
            into_response(result)
        }
        IpcRequest::Ping { message, device } => {
            let dev = resolve_device(&shared, device).await?;
            let ping_msg = message.unwrap_or_else(|| "Ping from Hyprconnect".to_string());
            let result = run_kdeconnect(&[
                "--device",
                &dev,
                "--ping-msg",
                &ping_msg,
            ])
            .await
            .map(|_| format!("Ping sent to {dev}"));
            into_response(result)
        }
        IpcRequest::Pair { device } => {
            let result = run_kdeconnect(&["--device", &device, "--pair"])
                .await
                .map(|_| format!("Pair request sent to {device}"));
            into_response(result)
        }
        IpcRequest::Unpair { device } => {
            let result = run_kdeconnect(&["--device", &device, "--unpair"])
                .await
                .map(|_| format!("Unpaired {device}"));
            into_response(result)
        }
        IpcRequest::Find { device } => {
            let dev = resolve_device(&shared, device).await?;
            let result = run_kdeconnect(&["--device", &dev, "--ring"])
                .await
                .map(|_| format!("Ringing {dev}"));
            into_response(result)
        }
        IpcRequest::RefreshNetwork => {
            let result = run_kdeconnect(&["--refresh"])
                .await
                .map(|_| "Refreshed KDE Connect device discovery".to_string());
            into_response(result)
        }
        IpcRequest::Mount { device } => {
            let dev = resolve_device(&shared, device).await?;
            let result = mount_device(&dev)
                .await
                .map(|mount| format!("Mounted {dev} at {mount}"));
            into_response(result)
        }
        IpcRequest::OpenMount { device } => {
            let dev = resolve_device(&shared, device).await?;
            let result = open_device_mount(&dev)
                .await
                .map(|mount| format!("Opened mount for {dev}: {mount}"));
            into_response(result)
        }
        IpcRequest::ToggleMount { device } => {
            let result = toggle_mount(&shared, device).await;
            into_response(result)
        }
        IpcRequest::Media { device, action } => {
            let result = handle_media_action(&shared, device, action).await;
            into_response(result)
        }
    };

    let body = serde_json::to_vec(&resp)?;
    stream.write_all(&body).await?;
    Ok(())
}

fn into_response(result: Result<String>) -> IpcResponse {
    match result {
        Ok(message) => IpcResponse {
            ok: true,
            message: Some(message),
            state: None,
        },
        Err(err) => IpcResponse {
            ok: false,
            message: Some(err.to_string()),
            state: None,
        },
    }
}

async fn refresh_state(shared: &Shared) -> Result<()> {
    if !command_exists("kdeconnect-cli").await {
        let next = DaemonState {
            devices: Vec::new(),
            updated_at: Some(Utc::now()),
        };
        *shared.state.write().await = next;
        return Ok(());
    }

    let prev = shared.state.read().await.clone();
    let names = list_devices().await?;
    let reachable = list_reachable_ids().await?;

    let mut devices = Vec::new();
    for (id, name) in names {
        let reach = reachable.contains(&id);
        let (mounted, mount_point) = read_mount_state_for(&id).await.unwrap_or((false, None));
        let (battery, charging) = read_battery_for(&id).await.unwrap_or((None, None));
        let (signal_percent, network_type) = read_connectivity_for(&id).await.unwrap_or((None, None));
        devices.push(DeviceState {
            id,
            name,
            reachable: reach,
            paired: true,
            mounted,
            mount_point,
            battery_percent: battery,
            charging,
            signal_percent,
            network_type,
        });
    }

    for id in reachable {
        if devices.iter().any(|d| d.id == id) {
            continue;
        }
        devices.push(DeviceState {
            name: id.clone(),
            id,
            reachable: true,
            paired: false,
            mounted: false,
            mount_point: None,
            battery_percent: None,
            charging: None,
            signal_percent: None,
            network_type: None,
        });
    }

    let next = DaemonState {
        devices,
        updated_at: Some(Utc::now()),
    };

    maybe_notify_connection_changes(shared, &prev, &next)?;
    *shared.state.write().await = next;
    Ok(())
}

fn maybe_notify_connection_changes(shared: &Shared, prev: &DaemonState, next: &DaemonState) -> Result<()> {
    if !shared.config.notifications_enabled {
        return Ok(());
    }

    let prev_map: HashMap<&str, bool> = prev
        .devices
        .iter()
        .map(|d| (d.id.as_str(), d.reachable))
        .collect();

    for d in &next.devices {
        let old = prev_map.get(d.id.as_str()).copied().unwrap_or(false);
        if old != d.reachable {
            let body = if d.reachable {
                "Phone connected"
            } else {
                "Phone disconnected"
            };
            let _ = Notification::new()
                .summary(&d.name)
                .body(body)
                .appname("Hyprconnect")
                .show();
        }
    }

    Ok(())
}

async fn share_path(shared: &Shared, value: &str, device: Option<String>) -> Result<String> {
    if value.trim().is_empty() {
        return Err(anyhow!("clipboard is empty"));
    }
    let dev = resolve_device(shared, device).await?;
    run_kdeconnect(&["--device", &dev, "--share", value]).await?;
    Ok(format!("Shared to {dev}"))
}

async fn resolve_device(shared: &Shared, requested: Option<String>) -> Result<String> {
    let state = shared.state.read().await;

    if let Some(id) = requested {
        let valid = state
            .devices
            .iter()
            .any(|d| d.id == id && d.paired && d.reachable);
        if !valid {
            return Err(anyhow!(
                "device '{id}' is not both paired and reachable"
            ));
        }
        return Ok(id);
    }

    if let Some(id) = &shared.config.default_device {
        let valid = state
            .devices
            .iter()
            .any(|d| d.id == *id && d.paired && d.reachable);
        if valid {
            return Ok(id.clone());
        }
    }

    if let Some(device) = state.devices.iter().find(|d| d.reachable && d.paired) {
        return Ok(device.id.clone());
    }

    Err(anyhow!(
        "no paired and reachable KDE Connect device found"
    ))
}

async fn read_clipboard() -> Result<String> {
    let output = Command::new("wl-paste")
        .arg("-n")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await
        .context("failed to execute wl-paste")?;

    if !output.status.success() {
        return Err(anyhow!("failed to read clipboard with wl-paste"));
    }

    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        return Err(anyhow!("clipboard is empty"));
    }
    Ok(text)
}

async fn list_devices() -> Result<Vec<(String, String)>> {
    let out = run_kdeconnect(&["--list-devices", "--id-name-only"]).await?;
    let legacy = Regex::new(r"^-\s*(?P<name>.+):\s*(?P<id>[A-Za-z0-9_-]+)$").unwrap();
    let current = Regex::new(r"^(?P<id>[A-Za-z0-9_-]+)\s+(?P<name>.+)$").unwrap();
    let mut devices = Vec::new();
    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if let Some(cap) = legacy.captures(line) {
            let id = cap["id"].to_string();
            let name = cap["name"].to_string();
            devices.push((id, name));
            continue;
        }

        if let Some(cap) = current.captures(line) {
            let id = cap["id"].to_string();
            let name = cap["name"].to_string();
            devices.push((id, name));
        }
    }
    Ok(devices)
}

async fn list_reachable_ids() -> Result<Vec<String>> {
    let out = run_kdeconnect(&["--list-available", "--id-only"]).await?;
    let ids = out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(ToString::to_string)
        .collect();
    Ok(ids)
}

async fn read_battery_for(device: &str) -> Result<(Option<u8>, Option<bool>)> {
    let charge = read_dbus_int_prop(device, "battery", "org.kde.kdeconnect.device.battery", "charge")
        .await?
        .and_then(|v| u8::try_from(v).ok());
    let charging = read_dbus_bool_prop(
        device,
        "battery",
        "org.kde.kdeconnect.device.battery",
        "isCharging",
    )
    .await?;
    Ok((charge, charging))
}

async fn read_connectivity_for(device: &str) -> Result<(Option<u8>, Option<String>)> {
    let strength = read_dbus_int_prop(
        device,
        "connectivity_report",
        "org.kde.kdeconnect.device.connectivity_report",
        "cellularNetworkStrength",
    )
    .await?
    .and_then(|v| u8::try_from(v).ok())
    .map(|bars| {
        if bars > 4 {
            100
        } else {
            bars.saturating_mul(25)
        }
    });

    let network_type = read_dbus_string_prop(
        device,
        "connectivity_report",
        "org.kde.kdeconnect.device.connectivity_report",
        "cellularNetworkType",
    )
    .await?;

    Ok((strength, network_type))
}

async fn read_mount_state_for(device: &str) -> Result<(bool, Option<String>)> {
    let mount_point = get_mount_point(device).await?;
    let mounted = mount_point
        .as_deref()
        .map(is_mountpoint_mounted)
        .unwrap_or(false);
    Ok((mounted, mount_point.filter(|p| !p.is_empty())))
}

async fn mount_device(device: &str) -> Result<String> {
    run_kdeconnect(&["--device", device, "--mount"]).await?;
    let path = wait_for_mount_point(device, true, Duration::from_millis(1400)).await?;
    if path.is_empty() {
        return Err(anyhow!("mount point is empty for device {device}"));
    }
    Ok(path)
}

async fn open_device_mount(device: &str) -> Result<String> {
    let mount = mount_device(device).await?;
    let target = internal_storage_path(&mount)?;
    Command::new("xdg-open")
        .arg(&target)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn xdg-open")?;
    Ok(target)
}

async fn toggle_mount(shared: &Shared, device: Option<String>) -> Result<String> {
    let dev = resolve_device(shared, device).await?;
    let mount_point = get_mount_point(&dev).await?;

    if let Some(path) = mount_point {
        if !path.is_empty() && is_mountpoint_mounted(&path) {
            unmount_path(&path).await?;
            wait_for_mount_state(&dev, false, Duration::from_millis(1400)).await?;
            let _ = refresh_state(shared).await;
            return Ok(format!("Unmounted {dev} from {path}"));
        }
    }

    let mount = open_device_mount(&dev).await?;
    let _ = refresh_state(shared).await;
    Ok(format!("Mounted and opened {dev}: {mount}"))
}

async fn unmount_path(path: &str) -> Result<()> {
    let fusermount = run_command_status("fusermount", &["-u", path]).await;
    if fusermount {
        return Ok(());
    }

    let umount = run_command_status("umount", &[path]).await;
    if umount {
        return Ok(());
    }

    Err(anyhow!("failed to unmount {path} with fusermount/umount"))
}

async fn get_mount_point(device: &str) -> Result<Option<String>> {
    let mount = match run_kdeconnect(&["--device", device, "--get-mount-point"]).await {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let path = mount.lines().next().unwrap_or_default().trim().to_string();
    if path.is_empty() {
        return Ok(None);
    }
    Ok(Some(path))
}

async fn wait_for_mount_state(device: &str, expected: bool, timeout: Duration) -> Result<()> {
    tokio::time::timeout(timeout, async {
        loop {
            let (mounted, _) = read_mount_state_for(device).await.unwrap_or((false, None));
            if mounted == expected {
                return Ok::<(), anyhow::Error>(());
            }
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| anyhow!("timed out waiting for mount state '{expected}' on device {device}"))??;
    Ok(())
}

async fn wait_for_mount_point(device: &str, must_be_mounted: bool, timeout: Duration) -> Result<String> {
    let path = tokio::time::timeout(timeout, async {
        loop {
            if let Some(path) = get_mount_point(device).await? {
                let mounted = is_mountpoint_mounted(&path);
                if mounted == must_be_mounted {
                    return Ok::<String, anyhow::Error>(path);
                }
            }
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| anyhow!("timed out waiting for mount point for device {device}"))??;
    Ok(path)
}

fn internal_storage_path(mount_point: &str) -> Result<String> {
    let target = format!("{mount_point}/storage/emulated/0");
    if !Path::new(&target).exists() {
        return Err(anyhow!(
            "internal storage path not found: {target}"
        ));
    }
    Ok(target)
}

async fn run_command_status(bin: &str, args: &[&str]) -> bool {
    Command::new(bin)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

fn is_mountpoint_mounted(path: &str) -> bool {
    let Ok(mounts) = std::fs::read_to_string("/proc/mounts") else {
        return false;
    };

    mounts.lines().any(|line| {
        let mut parts = line.split_whitespace();
        let _source = parts.next();
        let Some(target) = parts.next() else {
            return false;
        };
        target == path
    })
}

async fn listen_for_kdeconnect_events(shared: Shared) -> Result<()> {
    let conn = zbus::Connection::session().await?;
    let mut stream = zbus::MessageStream::from(&conn);
    let mut last_refresh = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .unwrap_or_else(Instant::now);

    while let Some(msg) = stream.next().await {
        let msg = msg?;
        let header = msg.header();
        if header.message_type() != MessageType::Signal {
            continue;
        }

        let Some(path) = header.path() else {
            continue;
        };
        let path = path.to_string();
        if !path.starts_with("/modules/kdeconnect/devices/") {
            continue;
        }

        let iface = header
            .interface()
            .map(|v| v.to_string())
            .unwrap_or_default();
        let member = header
            .member()
            .map(|v| v.to_string())
            .unwrap_or_default();

        if !is_refresh_signal(&iface, &member) {
            continue;
        }

        if last_refresh.elapsed() < Duration::from_millis(200) {
            continue;
        }
        last_refresh = Instant::now();

        if let Err(err) = refresh_state(&shared).await {
            eprintln!("event refresh failed: {err:#}");
        }
    }

    Ok(())
}

fn is_refresh_signal(interface: &str, member: &str) -> bool {
    if interface == "org.kde.kdeconnect.device" {
        return member == "reachableChanged" || member == "pairStateChanged";
    }
    if interface == "org.kde.kdeconnect.device.battery" {
        return member == "refreshed";
    }
    if interface == "org.kde.kdeconnect.device.connectivity_report" {
        return member == "refreshed";
    }
    false
}

async fn read_dbus_int_prop(
    device: &str,
    plugin: &str,
    interface: &str,
    prop: &str,
) -> Result<Option<i32>> {
    let path = format!("/modules/kdeconnect/devices/{device}/{plugin}");
    let out = run_busctl_get_property(&path, interface, prop).await?;
    let mut parts = out.split_whitespace();
    let _sig = parts.next();
    let value = parts
        .next()
        .and_then(|n| n.parse::<i32>().ok());
    Ok(value)
}

async fn read_dbus_bool_prop(
    device: &str,
    plugin: &str,
    interface: &str,
    prop: &str,
) -> Result<Option<bool>> {
    let path = format!("/modules/kdeconnect/devices/{device}/{plugin}");
    let out = run_busctl_get_property(&path, interface, prop).await?;
    let mut parts = out.split_whitespace();
    let _sig = parts.next();
    let value = parts
        .next()
        .and_then(|n| n.parse::<bool>().ok());
    Ok(value)
}

async fn read_dbus_string_prop(
    device: &str,
    plugin: &str,
    interface: &str,
    prop: &str,
) -> Result<Option<String>> {
    let path = format!("/modules/kdeconnect/devices/{device}/{plugin}");
    let out = run_busctl_get_property(&path, interface, prop).await?;
    let mut parts = out.splitn(2, ' ');
    let _sig = parts.next();
    let raw = parts.next().map(str::trim).unwrap_or("");
    if raw.is_empty() {
        return Ok(None);
    }
    let cleaned = raw.trim_matches('"').to_string();
    if cleaned.is_empty() {
        return Ok(None);
    }
    Ok(Some(cleaned))
}

async fn run_busctl_get_property(path: &str, interface: &str, prop: &str) -> Result<String> {
    if !command_exists("busctl").await {
        return Err(anyhow!("busctl not found"));
    }

    let out = Command::new("busctl")
        .args([
            "--user",
            "get-property",
            "org.kde.kdeconnect",
            path,
            interface,
            prop,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await
        .context("failed to execute busctl")?;

    if !out.status.success() {
        return Err(anyhow!("dbus property not available"));
    }

    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

async fn handle_media_action(
    shared: &Shared,
    device: Option<String>,
    action: MediaAction,
) -> Result<String> {
    let dev = resolve_device(shared, device).await?;
    let path = format!("/modules/kdeconnect/devices/{dev}/mprisremote");
    let iface = "org.kde.kdeconnect.device.mprisremote";

    match action {
        MediaAction::Status => media_status(&path, iface).await,
        MediaAction::PlayPause => {
            media_send_action(&path, "PlayPause").await?;
            Ok(format!("Sent PlayPause to {dev}"))
        }
        MediaAction::Next => {
            media_send_action(&path, "Next").await?;
            Ok(format!("Sent Next to {dev}"))
        }
        MediaAction::Previous => {
            media_send_action(&path, "Previous").await?;
            Ok(format!("Sent Previous to {dev}"))
        }
        MediaAction::Stop => {
            media_send_action(&path, "Stop").await?;
            Ok(format!("Sent Stop to {dev}"))
        }
        MediaAction::Seek { ms } => {
            media_seek(&path, ms).await?;
            Ok(format!("Seeked {dev} by {ms}ms"))
        }
        MediaAction::VolumeSet { value } => {
            media_set_volume(&path, value).await?;
            Ok(format!("Set phone media volume to {value}% on {dev}"))
        }
        MediaAction::PlayerList => media_player_list(&path, iface).await,
        MediaAction::PlayerSet { name } => {
            media_set_player(&path, &name).await?;
            Ok(format!("Set active phone player to '{name}'"))
        }
    }
}

async fn media_status(path: &str, iface: &str) -> Result<String> {
    let player = parse_dbus_string(&run_busctl_get_property(path, iface, "player").await?)
        .unwrap_or_else(|| "Unknown".to_string());
    let title = parse_dbus_string(&run_busctl_get_property(path, iface, "title").await?)
        .unwrap_or_else(|| "--".to_string());
    let artist = parse_dbus_string(&run_busctl_get_property(path, iface, "artist").await?)
        .unwrap_or_else(|| "--".to_string());
    let is_playing = parse_dbus_bool(&run_busctl_get_property(path, iface, "isPlaying").await?)
        .unwrap_or(false);
    let volume = parse_dbus_int(&run_busctl_get_property(path, iface, "volume").await?)
        .unwrap_or(0);

    Ok(format!(
        "Player: {player}\nState: {}\nTitle: {title}\nArtist: {artist}\nVolume: {volume}%",
        if is_playing { "Playing" } else { "Paused" }
    ))
}

async fn media_player_list(path: &str, iface: &str) -> Result<String> {
    let raw = run_busctl_get_property(path, iface, "playerList").await?;
    let players = parse_dbus_string_array(&raw);
    if players.is_empty() {
        return Ok("No phone media players reported".to_string());
    }
    Ok(format!("Players:\n{}", players.join("\n")))
}

async fn media_send_action(path: &str, action: &str) -> Result<()> {
    run_busctl_call(path, "org.kde.kdeconnect.device.mprisremote", "sendAction", &["s", action])
        .await
}

async fn media_seek(path: &str, ms: i32) -> Result<()> {
    run_busctl_call(
        path,
        "org.kde.kdeconnect.device.mprisremote",
        "seek",
        &["i", &ms.to_string()],
    )
    .await
}

async fn media_set_volume(path: &str, value: u8) -> Result<()> {
    run_busctl_set_property(
        path,
        "org.kde.kdeconnect.device.mprisremote",
        "volume",
        "i",
        &value.to_string(),
    )
    .await
}

async fn media_set_player(path: &str, name: &str) -> Result<()> {
    run_busctl_set_property(
        path,
        "org.kde.kdeconnect.device.mprisremote",
        "player",
        "s",
        name,
    )
    .await
}

async fn run_busctl_call(path: &str, interface: &str, member: &str, tail_args: &[&str]) -> Result<()> {
    if !command_exists("busctl").await {
        return Err(anyhow!("busctl not found"));
    }

    let mut args = vec![
        "--user",
        "call",
        "org.kde.kdeconnect",
        path,
        interface,
        member,
    ];
    args.extend_from_slice(tail_args);

    let out = Command::new("busctl")
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("failed to execute busctl call")?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if err.is_empty() {
            return Err(anyhow!("busctl call failed"));
        }
        return Err(anyhow!(err));
    }
    Ok(())
}

async fn run_busctl_set_property(
    path: &str,
    interface: &str,
    prop: &str,
    sig: &str,
    value: &str,
) -> Result<()> {
    if !command_exists("busctl").await {
        return Err(anyhow!("busctl not found"));
    }

    let out = Command::new("busctl")
        .args([
            "--user",
            "set-property",
            "org.kde.kdeconnect",
            path,
            interface,
            prop,
            sig,
            value,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("failed to execute busctl set-property")?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if err.is_empty() {
            return Err(anyhow!("busctl set-property failed"));
        }
        return Err(anyhow!(err));
    }
    Ok(())
}

fn parse_dbus_string(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.len() < 3 {
        return None;
    }
    let mut parts = trimmed.splitn(2, ' ');
    let _sig = parts.next()?;
    let val = parts.next()?.trim();
    let val = val.trim_matches('"').trim();
    if val.is_empty() {
        None
    } else {
        Some(val.to_string())
    }
}

fn parse_dbus_bool(raw: &str) -> Option<bool> {
    raw.split_whitespace().nth(1)?.parse::<bool>().ok()
}

fn parse_dbus_int(raw: &str) -> Option<i32> {
    raw.split_whitespace().nth(1)?.parse::<i32>().ok()
}

fn parse_dbus_string_array(raw: &str) -> Vec<String> {
    let re = Regex::new("\"([^\"]+)\"").unwrap();
    re.captures_iter(raw)
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
        .collect()
}

async fn run_kdeconnect(args: &[&str]) -> Result<String> {
    let out = Command::new("kdeconnect-cli")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("failed to execute kdeconnect-cli")?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if err.is_empty() {
            return Err(anyhow!("kdeconnect-cli failed"));
        }
        return Err(anyhow!(err));
    }

    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

async fn command_exists(name: &str) -> bool {
    Command::new("sh")
        .arg("-lc")
        .arg(format!("command -v {name} >/dev/null 2>&1"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}
