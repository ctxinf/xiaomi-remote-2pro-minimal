//! BLE Bridge for ATVV Voice Stream & Device State.
//!
//! Connects to Xiaomi Remote over BLE, subscribes to ATVV voice channels,
//! reads device information (model, battery), manages the ATVV session,
//! and feeds decoded PCM to the AudioSink.

use anyhow::Context as _;
use bluest::{Adapter, AdvertisingDevice, Characteristic, CharacteristicProperties, Device};
use futures_lite::StreamExt;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

use crate::atvv;
use crate::recorder::AudioSink;
use crate::session::{Action, AtvvSession};

pub const NAME_CANDIDATES: [&str; 4] = [
    "MI RC",
    "小米蓝牙语音遥控器",
    "Xiaomi Bluetooth Remote 2 Pro",
    "Xiaomi Bluetooth Remote",
];

pub fn name_matches(name: &str) -> bool {
    NAME_CANDIDATES.iter().any(|c| name == *c || name.starts_with(c))
}

pub fn advertisement_is_candidate(adv: &AdvertisingDevice) -> bool {
    if adv.adv_data.services.contains(&atvv::SERVICE) {
        return true;
    }
    adv.adv_data
        .local_name
        .as_deref()
        .map(name_matches)
        .unwrap_or(false)
}

#[derive(Debug, Clone)]
pub struct BridgeConfig {
    pub gain_db: f64,
    pub reconnect_delay: Duration,
    pub scan_timeout: Duration,
    pub target_name: Option<String>,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            gain_db: 0.0,
            reconnect_delay: Duration::from_secs(3),
            scan_timeout: Duration::from_secs(10),
            target_name: None,
        }
    }
}

pub async fn run_ble_bridge(
    sink: &mut dyn AudioSink,
    config: &BridgeConfig,
    shutdown: &CancellationToken,
) -> anyhow::Result<()> {
    let adapter = Adapter::default()
        .await
        .context("No Bluetooth adapter found on system")?;
    adapter
        .wait_available()
        .await
        .context("Bluetooth adapter is not available or turned off")?;

    log::info!("[BLE GATEWAY] Bluetooth adapter is ready.");

    while !shutdown.is_cancelled() {
        log::info!("[BLE GATEWAY] Searching for Xiaomi Remote...");
        match connect_and_run(&adapter, sink, config, shutdown).await {
            Ok(Ended::Shutdown) => {
                log::info!("[BLE GATEWAY] Clean shutdown requested.");
                return Ok(());
            }
            Ok(Ended::Disconnected) => {
                log::warn!(
                    "[BLE GATEWAY] Connection closed. Reconnecting in {:.0}s...",
                    config.reconnect_delay.as_secs_f64()
                );
            }
            Err(e) => {
                log::warn!(
                    "[BLE GATEWAY] Bridge error: {e:#}. Reconnecting in {:.0}s...",
                    config.reconnect_delay.as_secs_f64()
                );
            }
        }

        tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            _ = tokio::time::sleep(config.reconnect_delay) => {}
        }
    }

    Ok(())
}

enum Ended {
    Shutdown,
    Disconnected,
}

async fn find_remote(
    adapter: &Adapter,
    config: &BridgeConfig,
    shutdown: &CancellationToken,
) -> anyhow::Result<Option<Device>> {
    // 1. Check system connected devices exposing ATVV service
    match adapter.connected_devices_with_services(&[atvv::SERVICE]).await {
        Ok(devices) => {
            for device in devices {
                let name = device.name().unwrap_or_default();
                if let Some(target) = &config.target_name {
                    if !name.contains(target) {
                        continue;
                    }
                }
                log::info!(
                    "[BLE GATEWAY] Found remote among system connected devices: \"{}\" [{}]",
                    if name.is_empty() { "Xiaomi Remote" } else { &name },
                    device.id()
                );
                return Ok(Some(device));
            }
        }
        Err(e) => {
            log::debug!("[BLE GATEWAY] Failed checking connected devices: {e}");
        }
    }

    // 2. Scan BLE Advertisements
    log::info!(
        "[BLE GATEWAY] Scanning BLE advertisements ({:.0}s)...",
        config.scan_timeout.as_secs_f64()
    );
    let mut scan = adapter.scan(&[]).await.context("Failed to start BLE scan")?;
    let deadline = tokio::time::sleep(config.scan_timeout);
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(None),
            _ = &mut deadline => return Ok(None),
            item = scan.next() => match item {
                Some(adv) if advertisement_is_candidate(&adv) => {
                    let name = adv.adv_data.local_name.clone()
                        .or_else(|| adv.device.name().ok())
                        .unwrap_or_else(|| "MI RC".into());
                    if let Some(target) = &config.target_name {
                        if !name.contains(target) {
                            continue;
                        }
                    }
                    log::info!(
                        "[BLE GATEWAY] Discovered remote via scan: \"{}\" [{}] (RSSI: {:?})",
                        name,
                        adv.device.id(),
                        adv.rssi
                    );
                    return Ok(Some(adv.device));
                }
                Some(_) => {}
                None => return Ok(None),
            }
        }
    }
}

async fn read_device_info(device: &Device) {
    if let Ok(services) = device.discover_services_with_uuid(atvv::DEVICE_INFORMATION_SERVICE).await {
        if let Some(service) = services.into_iter().next() {
            if let Ok(chars) = service.discover_characteristics().await {
                for c in chars {
                    if c.uuid() == atvv::MODEL_NUMBER_CHARACTERISTIC {
                        if let Ok(val) = c.read().await {
                            let model = String::from_utf8_lossy(&val).trim_matches('\0').trim().to_string();
                            if !model.is_empty() {
                                log::info!("[BLE STATUS] Remote Model Number: {}", model);
                            }
                        }
                    }
                    if c.uuid() == atvv::FIRMWARE_REVISION_CHARACTERISTIC {
                        if let Ok(val) = c.read().await {
                            let fw = String::from_utf8_lossy(&val).trim_matches('\0').trim().to_string();
                            if !fw.is_empty() {
                                log::info!("[BLE STATUS] Firmware Revision: {}", fw);
                            }
                        }
                    }
                }
            }
        }
    }

    // Read battery level
    if let Ok(services) = device.discover_services_with_uuid(atvv::BATTERY_SERVICE).await {
        if let Some(service) = services.into_iter().next() {
            if let Ok(chars) = service.discover_characteristics_with_uuid(atvv::BATTERY_LEVEL_CHARACTERISTIC).await {
                if let Some(c) = chars.into_iter().next() {
                    if let Ok(val) = c.read().await {
                        if let Some(&level) = val.first() {
                            log::info!("[BLE STATUS] Battery Level: {}%", level);
                        }
                    }
                }
            }
        }
    }
}

async fn write_tx(
    tx: &Characteristic,
    props: Option<&CharacteristicProperties>,
    bytes: &[u8],
) -> bluest::Result<()> {
    let without_response = props.map(|p| p.write_without_response).unwrap_or(false);
    if without_response {
        tx.write_without_response(bytes).await
    } else {
        tx.write(bytes).await
    }
}

async fn connect_and_run(
    adapter: &Adapter,
    sink: &mut dyn AudioSink,
    config: &BridgeConfig,
    shutdown: &CancellationToken,
) -> anyhow::Result<Ended> {
    let Some(device) = find_remote(adapter, config, shutdown).await? else {
        if shutdown.is_cancelled() {
            return Ok(Ended::Shutdown);
        }
        anyhow::bail!("Remote not found. Please ensure it is paired or long-press Home+Menu to enter pairing mode.");
    };

    let dev_name = device.name().unwrap_or_else(|_| "Xiaomi Remote".into());
    log::info!("[BLE STATUS] Connecting to device: \"{}\" [{}]...", dev_name, device.id());

    adapter
        .connect_device(&device)
        .await
        .context("Failed to establish BLE connection")?;

    log::info!("[BLE STATUS] Connected successfully: \"{}\" [{}]", dev_name, device.id());

    // Read device information and battery
    read_device_info(&device).await;

    // Discover ATVV Service
    let services = device
        .discover_services_with_uuid(atvv::SERVICE)
        .await
        .context("Failed to discover ATVV Service")?;
    let service = services
        .into_iter()
        .next()
        .context("Device does not provide ATVV Voice Service")?;

    let characteristics = service
        .discover_characteristics()
        .await
        .context("Failed to discover ATVV Characteristics")?;

    let find_char = |uuid| characteristics.iter().find(|c| c.uuid() == uuid).cloned();
    let tx = find_char(atvv::CHAR_TX).context("Missing ATVV TX characteristic")?;
    let audio = find_char(atvv::CHAR_AUDIO).context("Missing ATVV AUDIO characteristic")?;
    let ctl = find_char(atvv::CHAR_CTL).context("Missing ATVV CTL characteristic")?;

    let mut ctl_stream = ctl
        .notify()
        .await
        .context("Failed to subscribe to ATVV CTL notifications")?;
    let mut audio_stream = audio
        .notify()
        .await
        .context("Failed to subscribe to ATVV AUDIO notifications")?;

    let tx_props = tx.properties().await.ok();
    let mut session = AtvvSession::new(config.gain_db);

    // Send GET_CAPS
    write_tx(&tx, tx_props.as_ref(), &atvv::GET_CAPS)
        .await
        .context("Failed to send GET_CAPS to remote")?;

    log::info!("[BLE ATVV] GET_CAPS sent. Ready! Press remote Mic button to record voice.");

    let start_instant = Instant::now();
    let mut actions: Vec<Action> = Vec::new();

    let ended = loop {
        tokio::select! {
            _ = shutdown.cancelled() => break Ended::Shutdown,
            item = ctl_stream.next() => match item {
                Some(Ok(data)) => {
                    let elapsed_ms = start_instant.elapsed().as_millis() as u64;
                    session.handle_control(&data, elapsed_ms, &mut actions);
                }
                Some(Err(e)) => {
                    log::warn!("[BLE ATVV] CTL notification error: {e}");
                    break Ended::Disconnected;
                }
                None => {
                    log::warn!("[BLE ATVV] CTL stream closed by remote.");
                    break Ended::Disconnected;
                }
            },
            item = audio_stream.next() => match item {
                Some(Ok(data)) => {
                    let elapsed_ms = start_instant.elapsed().as_millis() as u64;
                    session.handle_audio(&data, elapsed_ms, &mut actions);
                }
                Some(Err(e)) => {
                    log::warn!("[BLE ATVV] AUDIO notification error: {e}");
                    break Ended::Disconnected;
                }
                None => {
                    log::warn!("[BLE ATVV] AUDIO stream closed by remote.");
                    break Ended::Disconnected;
                }
            }
        }

        let mut fatal_err: Option<String> = None;
        for action in actions.drain(..) {
            match action {
                Action::SendTx(bytes) => {
                    if let Err(e) = write_tx(&tx, tx_props.as_ref(), &bytes).await {
                        log::warn!("[BLE ATVV] Failed to write TX: {e}");
                    }
                }
                Action::StreamStarted => {
                    log::info!("[BLE ATVV] Voice streaming session STARTED.");
                    sink.on_stream_start();
                }
                Action::Pcm(samples) => {
                    sink.push(&samples);
                }
                Action::StreamStopped => {
                    log::info!("[BLE ATVV] Voice streaming session STOPPED.");
                    sink.on_stream_stop();
                }
                Action::Fatal(reason) => {
                    fatal_err = Some(reason);
                }
            }
        }

        if let Some(err) = fatal_err {
            log::error!("[BLE ATVV] Fatal protocol error: {err}");
            break Ended::Disconnected;
        }
    };

    // Clean up session and microphone
    session.finish(&mut actions);
    for action in actions.drain(..) {
        if let Action::StreamStopped = action {
            sink.on_stream_stop();
        }
    }

    if let Some(close_pkt) = session.take_mic_close() {
        let _ = write_tx(&tx, tx_props.as_ref(), &close_pkt).await;
    }

    let _ = adapter.disconnect_device(&device).await;
    log::info!("[BLE STATUS] Disconnected from device.");

    Ok(ended)
}
