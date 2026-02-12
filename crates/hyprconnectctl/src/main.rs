use anyhow::{anyhow, Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{generate, Shell};
use hyprconnect_core::{
    runtime_socket_path, DaemonState, IpcRequest, IpcResponse, MediaAction, WaybarPayload,
};
use std::io;
use std::process::Stdio;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::process::Command;

#[derive(Debug, Parser)]
#[command(
    name = "hyprconnectctl",
    version,
    about = "Control and inspect Hyprconnect",
    long_about = "hyprconnectctl talks to the local hyprconnectd daemon over a Unix socket.\nIt provides device status, pairing operations, sharing actions, ping, diagnostics,\nand Waybar-formatted JSON output."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    #[command(
        about = "Show concise daemon status",
        long_about = "Print a human-readable summary of all devices currently known by hyprconnectd, including reachability, pairing state, and battery percentage."
    )]
    Status,
    #[command(
        about = "List all known devices",
        long_about = "List every device present in the daemon cache. Use --json for machine-readable output that includes battery, charging, signal, and network metadata."
    )]
    Devices {
        #[arg(
            long,
            help = "Emit structured JSON instead of plain text",
            long_help = "Emit pretty-printed JSON for all known devices. This is useful for scripts and debugging."
        )]
        json: bool,
    },
    #[command(
        about = "List currently reachable devices",
        long_about = "Show only devices that are currently reachable over KDE Connect. Reachable means online and visible to the local host."
    )]
    ListAvailable {
        #[arg(
            long,
            help = "Emit structured JSON instead of plain text",
            long_help = "Emit pretty-printed JSON for reachable devices only."
        )]
        json: bool,
    },
    #[command(
        about = "Request pairing with a device",
        long_about = "Send a KDE Connect pairing request to a specific device id.\nYou may need to accept the request on the phone."
    )]
    Pair {
        #[arg(
            long,
            help = "Device id to pair with",
            long_help = "KDE Connect device id. Obtain it from `hyprconnectctl devices` or `hyprconnectctl list-available`."
        )]
        device: String,
    },
    #[command(
        about = "Unpair a device",
        long_about = "Remove KDE Connect pairing from a specific device id.\nThis does not delete local binaries, only pairing state."
    )]
    Unpair {
        #[arg(
            long,
            help = "Device id to unpair",
            long_help = "KDE Connect device id to unpair from this host."
        )]
        device: String,
    },
    #[command(
        about = "Run local diagnostics",
        long_about = "Run prerequisite checks for Hyprconnect and KDE Connect, including binary availability and daemon socket health."
    )]
    Doctor,
    #[command(
        about = "Request device rediscovery",
        long_about = "Ask KDE Connect to rescan and re-establish device discovery on the local network."
    )]
    Refresh,
    #[command(
        about = "Ring the target phone",
        long_about = "Trigger KDE Connect find-my-phone behavior on a paired and reachable target device."
    )]
    Find {
        #[arg(
            long,
            help = "Target device id",
            long_help = "Optional device id override. If omitted, hyprconnect chooses default_device, then first paired+reachable device."
        )]
        device: Option<String>,
    },
    #[command(
        about = "Mount phone filesystem",
        long_about = "Request KDE Connect SFTP mount for a paired and reachable target device."
    )]
    Mount {
        #[arg(
            long,
            help = "Target device id",
            long_help = "Optional device id override. If omitted, hyprconnect chooses default_device, then first paired+reachable device."
        )]
        device: Option<String>,
    },
    #[command(
        about = "Open mounted phone filesystem",
        long_about = "Mount target device filesystem with KDE Connect if needed, then open mountpoint with xdg-open."
    )]
    OpenMount {
        #[arg(
            long,
            help = "Target device id",
            long_help = "Optional device id override. If omitted, hyprconnect chooses default_device, then first paired+reachable device."
        )]
        device: Option<String>,
    },
    #[command(
        about = "Toggle phone filesystem mount",
        long_about = "If target device storage is mounted, unmount it. Otherwise mount and open it with xdg-open."
    )]
    ToggleMount {
        #[arg(
            long,
            help = "Target device id",
            long_help = "Optional device id override. If omitted, hyprconnect chooses default_device, then first paired+reachable device."
        )]
        device: Option<String>,
    },
    #[command(
        about = "Emit Waybar JSON payload",
        long_about = "Output a single JSON object suitable for Waybar custom modules.\nThe payload contains text, class, and tooltip fields."
    )]
    WaybarJson,
    #[command(
        about = "Share a file to a device",
        long_about = "Send a local file path to a paired and reachable device using KDE Connect share plugin."
    )]
    ShareFile {
        #[arg(
            help = "Path to a file to share",
            long_help = "Path to a local file that will be sent through KDE Connect share plugin."
        )]
        path: String,
        #[arg(
            long,
            help = "Target device id",
            long_help = "Optional device id override. If omitted, hyprconnect chooses default_device, then first paired+reachable device."
        )]
        device: Option<String>,
    },
    #[command(
        about = "Share a URL to a device",
        long_about = "Send a URL to a paired and reachable device using KDE Connect share plugin."
    )]
    ShareUrl {
        #[arg(
            help = "URL to share",
            long_help = "URL string to send to the target device."
        )]
        url: String,
        #[arg(
            long,
            help = "Target device id",
            long_help = "Optional device id override. If omitted, hyprconnect chooses default_device, then first paired+reachable device."
        )]
        device: Option<String>,
    },
    #[command(
        about = "Share clipboard text or URL",
        long_about = "Read current Wayland clipboard contents via wl-paste and share them to a device through KDE Connect."
    )]
    ShareClipboard {
        #[arg(
            long,
            help = "Target device id",
            long_help = "Optional device id override. If omitted, hyprconnect chooses default_device, then first paired+reachable device."
        )]
        device: Option<String>,
    },
    #[command(
        about = "Ping a device",
        long_about = "Send a ping notification to a paired and reachable device.\nUse --message to customize displayed text on the phone."
    )]
    Ping {
        #[arg(
            long,
            help = "Target device id",
            long_help = "Optional device id override. If omitted, hyprconnect chooses default_device, then first paired+reachable device."
        )]
        device: Option<String>,
        #[arg(
            long,
            help = "Custom ping message",
            long_help = "Optional message displayed by KDE Connect on the target device."
        )]
        message: Option<String>,
    },
    #[command(
        about = "Control phone media playback",
        long_about = "Control media on the connected phone through KDE Connect mprisremote plugin."
    )]
    Media {
        #[arg(
            long,
            help = "Target device id",
            long_help = "Optional device id override. If omitted, hyprconnect chooses default_device, then first paired+reachable device."
        )]
        device: Option<String>,
        #[command(subcommand)]
        command: MediaCommands,
    },
    #[command(
        about = "Generate shell completion script",
        long_about = "Print shell completion script to stdout for a chosen shell.\nUse with redirection to install completion files."
    )]
    Completions {
        #[arg(
            long,
            help = "Shell to generate completions for",
            long_help = "Target shell. Supported values: bash, zsh, fish, elvish, powershell."
        )]
        shell: Shell,
    },
}

#[derive(Debug, Subcommand)]
enum MediaCommands {
    #[command(about = "Show phone media status")]
    Status,
    #[command(about = "Toggle play/pause")]
    PlayPause,
    #[command(about = "Skip to next track")]
    Next,
    #[command(about = "Go to previous track")]
    Previous,
    #[command(about = "Stop playback")]
    Stop,
    #[command(about = "Seek by milliseconds (negative allowed)")]
    Seek {
        #[arg(long, help = "Seek delta in milliseconds")]
        ms: i32,
    },
    #[command(about = "Set phone media volume (0-100)")]
    Volume {
        #[arg(long, help = "Absolute volume percent", value_parser = clap::value_parser!(u8).range(0..=100))]
        set: u8,
    },
    #[command(about = "List available phone media players")]
    PlayerList,
    #[command(about = "Set active phone media player")]
    PlayerSet {
        #[arg(long, help = "Player name as listed by player-list")]
        name: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Status => {
            let resp = send(IpcRequest::GetState).await?;
            let state = resp.state.ok_or_else(|| anyhow!("daemon returned no state"))?;
            print_status(&state);
        }
        Commands::Devices { json } => {
            let resp = send(IpcRequest::GetState).await?;
            let state = resp.state.ok_or_else(|| anyhow!("daemon returned no state"))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&state.devices)?);
            } else {
                for d in &state.devices {
                    let conn = if d.reachable { "connected" } else { "offline" };
                    let batt = d
                        .battery_percent
                        .map(|v| format!("{v}%"))
                        .unwrap_or_else(|| "n/a".to_string());
                    println!("{} ({}) - {} - battery {}", d.name, d.id, conn, batt);
                }
            }
        }
        Commands::ListAvailable { json } => {
            let resp = send(IpcRequest::GetState).await?;
            let state = resp.state.ok_or_else(|| anyhow!("daemon returned no state"))?;
            let available: Vec<_> = state.devices.into_iter().filter(|d| d.reachable).collect();
            if json {
                println!("{}", serde_json::to_string_pretty(&available)?);
            } else if available.is_empty() {
                println!("No reachable devices found");
            } else {
                for d in available {
                    let pair = if d.paired { "paired" } else { "unpaired" };
                    println!("{} ({}) - {}", d.name, d.id, pair);
                }
            }
        }
        Commands::Pair { device } => {
            print_message(send(IpcRequest::Pair { device }).await?);
        }
        Commands::Unpair { device } => {
            print_message(send(IpcRequest::Unpair { device }).await?);
        }
        Commands::Doctor => {
            run_doctor().await;
        }
        Commands::Refresh => {
            print_message(send(IpcRequest::RefreshNetwork).await?);
        }
        Commands::Find { device } => {
            print_message(send(IpcRequest::Find { device }).await?);
        }
        Commands::Mount { device } => {
            print_message(send(IpcRequest::Mount { device }).await?);
        }
        Commands::OpenMount { device } => {
            print_message(send(IpcRequest::OpenMount { device }).await?);
        }
        Commands::ToggleMount { device } => {
            print_message(send(IpcRequest::ToggleMount { device }).await?);
        }
        Commands::WaybarJson => {
            let resp = send(IpcRequest::GetState).await?;
            let state = resp.state.ok_or_else(|| anyhow!("daemon returned no state"))?;
            let payload = build_waybar_payload(&state);
            println!("{}", serde_json::to_string(&payload)?);
        }
        Commands::ShareFile { path, device } => {
            print_message(send(IpcRequest::ShareFile { path, device }).await?);
        }
        Commands::ShareUrl { url, device } => {
            print_message(send(IpcRequest::ShareUrl { url, device }).await?);
        }
        Commands::ShareClipboard { device } => {
            print_message(send(IpcRequest::ShareClipboard { device }).await?);
        }
        Commands::Ping { device, message } => {
            print_message(send(IpcRequest::Ping { message, device }).await?);
        }
        Commands::Media { device, command } => {
            let action = match command {
                MediaCommands::Status => MediaAction::Status,
                MediaCommands::PlayPause => MediaAction::PlayPause,
                MediaCommands::Next => MediaAction::Next,
                MediaCommands::Previous => MediaAction::Previous,
                MediaCommands::Stop => MediaAction::Stop,
                MediaCommands::Seek { ms } => MediaAction::Seek { ms },
                MediaCommands::Volume { set } => MediaAction::VolumeSet { value: set },
                MediaCommands::PlayerList => MediaAction::PlayerList,
                MediaCommands::PlayerSet { name } => MediaAction::PlayerSet { name },
            };
            print_message(send(IpcRequest::Media { device, action }).await?);
        }
        Commands::Completions { shell } => {
            let mut cmd = Cli::command();
            generate(shell, &mut cmd, "hyprconnectctl", &mut io::stdout());
        }
    }

    Ok(())
}

fn print_status(state: &DaemonState) {
    println!("Hyprconnect devices: {}", state.devices.len());
    for d in &state.devices {
        let conn = if d.reachable { "connected" } else { "offline" };
        let pair = if d.paired { "paired" } else { "unpaired" };
        let batt = d
            .battery_percent
            .map(|v| format!("{v}%"))
            .unwrap_or_else(|| "n/a".to_string());
        println!("- {} ({}) :: {} / {} :: battery {}", d.name, d.id, conn, pair, batt);
    }
}

fn print_message(resp: IpcResponse) {
    if resp.ok {
        println!("{}", resp.message.unwrap_or_else(|| "ok".to_string()));
    } else {
        eprintln!("{}", resp.message.unwrap_or_else(|| "action failed".to_string()));
        std::process::exit(1);
    }
}

fn build_waybar_payload(state: &DaemonState) -> WaybarPayload {
    let connected = state.devices.iter().filter(|d| d.reachable).count();
    if connected == 0 {
        return WaybarPayload {
            text: "󰄰".to_string(),
            tooltip: "Phone: offline".to_string(),
            class: "disconnected".to_string(),
        };
    }

    let device = state
        .devices
        .iter()
        .find(|d| d.reachable)
        .or_else(|| state.devices.first());

    if let Some(d) = device {
        let battery_percent = d.battery_percent;
        let battery = battery_percent
            .map(|v| format!("{v}%"))
            .unwrap_or_else(|| "--".to_string());
        let signal_icon = cellular_signal_icon(d.signal_percent);
        let mount_suffix = if d.mounted { " 󰛳" } else { "" };
        let charge_suffix = if d.charging == Some(true) { " " } else { "" };
        let text = format!("{signal_icon} 󰄜{mount_suffix} {battery}{charge_suffix}");

        let class = match battery_percent {
            Some(b) if b < 30 => "crit",
            Some(b) if b < 50 => "warn",
            _ => "ok",
        }
        .to_string();

        let signal_text = d
            .signal_percent
            .map(|v| format!("{v}%"))
            .unwrap_or_else(|| "--".to_string());
        let network_type = d.network_type.as_deref().unwrap_or("Unknown");
        let mount_status = if d.mounted { "Yes" } else { "No" };
        let mount_point = if d.mounted {
            d.mount_point.as_deref().unwrap_or("--")
        } else {
            "--"
        };

        let tooltip = format!(
            "{}\nBattery: {}\nStatus: {}\nPaired: {}\nMounted: {}\nMount point: {}\nSignal: {}\nNetwork: {}\nDevices connected: {}",
            d.name,
            battery,
            if d.reachable { "Connected" } else { "Offline" },
            if d.paired { "Yes" } else { "No" },
            mount_status,
            mount_point,
            signal_text,
            network_type,
            connected,
        );

        return WaybarPayload {
            text,
            tooltip,
            class,
        };
    }

    WaybarPayload {
        text: "󰄰".to_string(),
        tooltip: "Phone: unavailable".to_string(),
        class: "disconnected".to_string(),
    }
}

fn cellular_signal_icon(signal_percent: Option<u8>) -> &'static str {
    match signal_percent {
        Some(v) if v >= 75 => "󰣺",
        Some(v) if v >= 50 => "󰣸",
        Some(v) if v >= 30 => "󰣶",
        Some(v) if v >= 10 => "󰣴",
        _ => "󰣾",
    }
}

async fn send(req: IpcRequest) -> Result<IpcResponse> {
    let socket = runtime_socket_path()?;
    let mut stream = UnixStream::connect(&socket)
        .await
        .with_context(|| format!("hyprconnectd is not running ({})", socket.display()))?;

    let body = serde_json::to_vec(&req)?;
    stream.write_all(&body).await?;
    stream.shutdown().await?;

    let mut resp = Vec::new();
    stream.read_to_end(&mut resp).await?;
    let parsed: IpcResponse = serde_json::from_slice(&resp).context("invalid daemon response")?;
    Ok(parsed)
}

async fn run_doctor() {
    let mut all_ok = true;

    let kdeconnect_cli = command_exists("kdeconnect-cli").await;
    report("kdeconnect-cli", kdeconnect_cli);
    all_ok = all_ok && kdeconnect_cli;

    let kdeconnectd = command_exists("kdeconnectd").await;
    report("kdeconnectd", kdeconnectd);
    all_ok = all_ok && kdeconnectd;

    let wl_paste = command_exists("wl-paste").await;
    report("wl-paste", wl_paste);

    let daemon_up = send(IpcRequest::GetState).await.is_ok();
    report("hyprconnectd socket", daemon_up);
    all_ok = all_ok && daemon_up;

    if kdeconnect_cli {
        let cli_ok = run_cmd_ok("kdeconnect-cli", &["--list-devices"]).await;
        report("kdeconnect-cli list-devices", cli_ok);
        all_ok = all_ok && cli_ok;
    }

    if daemon_up {
        if let Ok(resp) = send(IpcRequest::GetState).await {
            if let Some(state) = resp.state {
                if let Some(device) = state.devices.iter().find(|d| d.reachable).or_else(|| state.devices.first()) {
                    let mprisremote = plugin_supported(&device.id, "kdeconnect_mprisremote").await;
                    let mpriscontrol = plugin_supported(&device.id, "kdeconnect_mpriscontrol").await;
                    let systemvolume = plugin_supported(&device.id, "kdeconnect_systemvolume").await;
                    report("plugin mprisremote", mprisremote);
                    report("plugin mpriscontrol", mpriscontrol);
                    report("plugin systemvolume", systemvolume);
                }
            }
        }
    }

    if all_ok {
        println!("Doctor: ready for pairing and connect");
    } else {
        println!("Doctor: fix failed checks above, then retry");
    }
}

async fn plugin_supported(device_id: &str, plugin_name: &str) -> bool {
    let out = Command::new("busctl")
        .args([
            "--user",
            "get-property",
            "org.kde.kdeconnect",
            &format!("/modules/kdeconnect/devices/{device_id}"),
            "org.kde.kdeconnect.device",
            "supportedPlugins",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await;

    let Ok(out) = out else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    raw.contains(plugin_name)
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

async fn run_cmd_ok(bin: &str, args: &[&str]) -> bool {
    Command::new(bin)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

fn report(label: &str, ok: bool) {
    let status = if ok { "ok" } else { "missing/fail" };
    println!("{label}: {status}");
}
