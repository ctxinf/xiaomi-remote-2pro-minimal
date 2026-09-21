//! BLE Bridge for ATVV Voice Stream & Device State.
//!
//! Connects to Xiaomi Remote over BLE, subscribes to ATVV voice channels,
//! reads device information (model, battery), manages the ATVV session,
//! and feeds decoded PCM to the AudioSink.

use anyhow::Context as _;
use bluest::{Adapter, AdvertisingDevice, Characteristic, CharacteristicProperties, ConnectionEvent, Device};
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

/// Time to let BlueZ actually stop discovery after the scan stream is dropped.
const SCAN_SETTLE_DELAY: Duration = Duration::from_millis(500);

/// Time to let BlueZ finish tearing down the previous notification session
/// (`StopNotify` is issued asynchronously when the stream is dropped) before
/// subscribing again. Re-subscribing too early can be folded into the dying
/// session, which is exactly the stale-subscription state we are escaping.
const RESUBSCRIBE_SETTLE_DELAY: Duration = Duration::from_millis(300);

/// How often the link-health state machine runs. Cheap: a timer tick that
/// usually does nothing.
const HEALTH_TICK: Duration = Duration::from_secs(1);

/// The remote answers `GET_CAPS` in well under 100 ms on a healthy link
/// (measured: 25-40 ms), so anything beyond this means the notification
/// subscription is not actually live.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// Same deadline, applied to the idle keep-alive probe.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// A connection-event stream that dies sooner than this never worked in the
/// first place; treating that as a disconnect would spin the reconnect loop.
const WATCHER_MIN_LIFETIME: Duration = Duration::from_secs(2);

/// After this much silence on both ATVV channels, probe the link instead of
/// trusting it. A remote nobody is touching is legitimately silent for hours,
/// so silence alone is never treated as failure.
const IDLE_PROBE_INTERVAL: Duration = Duration::from_secs(60);

/// Upper bound for the reconnect backoff when the remote is simply gone
/// (out of range, batteries out, left at home).
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(60);

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

    // Consecutive rounds that never reached a working ATVV session. Used only
    // to stretch the retry delay so a remote that is out of range for hours
    // does not fill the journal with one failure every few seconds.
    let mut failed_rounds: u32 = 0;

    // The remote we last talked to. Lets the retry wait watch that one device
    // come back instead of blindly sleeping out the backoff.
    let mut known_device: Option<bluest::DeviceId> = None;

    while !shutdown.is_cancelled() {
        // Re-checked every round, not once at startup: the adapter goes away
        // on rfkill, suspend, and USB re-enumeration, and comes back later.
        if !wait_adapter_available(&adapter, shutdown).await {
            return Ok(());
        }

        log::info!("[BLE GATEWAY] Searching for Xiaomi Remote...");
        match connect_and_run(&adapter, sink, config, shutdown, &mut known_device).await {
            Ok(Ended::Shutdown) => {
                log::info!("[BLE GATEWAY] Clean shutdown requested.");
                return Ok(());
            }
            Ok(Ended::Disconnected { was_live }) => {
                if was_live {
                    failed_rounds = 0;
                } else {
                    failed_rounds = failed_rounds.saturating_add(1);
                }
                log::warn!(
                    "[BLE GATEWAY] Session ended. Reconnecting in {:.0}s...",
                    backoff_delay(config, failed_rounds).as_secs_f64()
                );
            }
            Err(e) => {
                failed_rounds = failed_rounds.saturating_add(1);
                log::warn!(
                    "[BLE GATEWAY] Bridge error: {e:#}. Reconnecting in {:.0}s...",
                    backoff_delay(config, failed_rounds).as_secs_f64()
                );
            }
        }

        if !wait_before_retry(
            &adapter,
            backoff_delay(config, failed_rounds),
            config.reconnect_delay,
            known_device.as_ref(),
            shutdown,
        )
        .await
        {
            return Ok(());
        }
    }

    Ok(())
}

/// Waits out the retry delay, but cuts it short as soon as the remote we know
/// about is back on the system bus.
///
/// Without this, a remote that reappears during a stretched backoff would stay
/// unusable for up to a minute — long enough to swallow the first mic press
/// after the user walks back to the machine. `floor` is always waited out first
/// so BlueZ can finish tearing down the previous GATT session.
///
/// Returns `false` if shutdown was requested.
async fn wait_before_retry(
    adapter: &Adapter,
    total: Duration,
    floor: Duration,
    known_device: Option<&bluest::DeviceId>,
    shutdown: &CancellationToken,
) -> bool {
    let floor = floor.min(total);
    tokio::select! {
        _ = shutdown.cancelled() => return false,
        _ = tokio::time::sleep(floor) => {}
    }

    let Some(id) = known_device else {
        tokio::select! {
            _ = shutdown.cancelled() => return false,
            _ = tokio::time::sleep(total.saturating_sub(floor)) => {}
        }
        return true;
    };

    let deadline = Instant::now() + total.saturating_sub(floor);
    while Instant::now() < deadline {
        if let Ok(device) = adapter.open_device(id).await {
            if device.is_connected().await {
                log::info!("[BLE GATEWAY] Remote is back on the system bus; retrying now.");
                return true;
            }
        }
        tokio::select! {
            _ = shutdown.cancelled() => return false,
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
        }
    }
    true
}

/// Doubles the configured delay per consecutive failure, capped. Returns the
/// base delay again as soon as a round manages to bring ATVV up.
fn backoff_delay(config: &BridgeConfig, failed_rounds: u32) -> Duration {
    let factor = 1u32 << failed_rounds.min(5);
    (config.reconnect_delay * factor).min(MAX_RECONNECT_DELAY)
}

/// Waits for the adapter, logging the transition once rather than per retry.
/// Returns `false` if shutdown was requested while waiting.
async fn wait_adapter_available(adapter: &Adapter, shutdown: &CancellationToken) -> bool {
    let mut announced = false;
    loop {
        let wait = tokio::select! {
            _ = shutdown.cancelled() => return false,
            res = adapter.wait_available() => res,
        };
        match wait {
            Ok(()) => {
                if announced {
                    log::info!("[BLE GATEWAY] Bluetooth adapter is back.");
                } else {
                    log::debug!("[BLE GATEWAY] Bluetooth adapter is ready.");
                }
                return true;
            }
            Err(e) => {
                if !announced {
                    log::warn!(
                        "[BLE GATEWAY] Bluetooth adapter unavailable ({e}); waiting for it to come back. \
                         If this persists, check `rfkill list`."
                    );
                    announced = true;
                }
                tokio::select! {
                    _ = shutdown.cancelled() => return false,
                    _ = tokio::time::sleep(Duration::from_secs(2)) => {}
                }
            }
        }
    }
}

enum Ended {
    Shutdown,
    /// The round is over and the outer loop should rebuild everything.
    /// `was_live` records whether ATVV ever came up, which decides the backoff.
    Disconnected { was_live: bool },
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
                    let device = adv.device;
                    // Stop discovery before connecting: BlueZ aborts LE connections
                    // (le-connection-abort-by-local) if a scan is still active, and
                    // dropping the stream only issues StopDiscovery asynchronously.
                    drop(scan);
                    tokio::time::sleep(SCAN_SETTLE_DELAY).await;
                    return Ok(Some(device));
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
    known_device: &mut Option<bluest::DeviceId>,
) -> anyhow::Result<Ended> {
    let Some(device) = find_remote(adapter, config, shutdown).await? else {
        if shutdown.is_cancelled() {
            return Ok(Ended::Shutdown);
        }
        anyhow::bail!("Remote not found. Please ensure it is paired or long-press Home+Menu to enter pairing mode.");
    };

    *known_device = Some(device.id());

    let dev_name = device.name().unwrap_or_else(|_| "Xiaomi Remote".into());

    // A remote that is already connected belongs to the system (it is a paired
    // HID keyboard the user is navigating with). Remember that, so the cleanup
    // path never tears down a link this process did not open.
    let adopted_existing_link = device.is_connected().await;

    if adopted_existing_link {
        log::info!("[BLE STATUS] Adopting existing system link: \"{}\" [{}]", dev_name, device.id());
    } else {
        log::info!("[BLE STATUS] Connecting to device: \"{}\" [{}]...", dev_name, device.id());
    }

    adapter
        .connect_device(&device)
        .await
        .context("Failed to establish BLE connection")?;

    log::info!("[BLE STATUS] Connected successfully: \"{}\" [{}]", dev_name, device.id());

    // Subscribe to link state *before* the handshake, so a drop that happens
    // during service discovery is not missed.
    let mut conn_events = match adapter.device_connection_events(&device).await {
        Ok(events) => Some(events),
        Err(e) => {
            // Not fatal: the idle probe below still catches a dead link, just
            // up to IDLE_PROBE_INTERVAL later.
            log::warn!("[BLE STATUS] Cannot watch connection events ({e}); relying on the keep-alive probe.");
            None
        }
    };

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

    // Fresh subscriptions on every round. This is the whole point of tearing
    // the session down: after an unexpected reconnect BlueZ keeps reporting
    // `Notifying = true` without ever re-writing the CCCD to the remote, so the
    // only way back to a notifying device is a new StartNotify.
    tokio::time::sleep(RESUBSCRIBE_SETTLE_DELAY).await;
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

    log::info!("[BLE ATVV] GET_CAPS sent, waiting for the remote to confirm capabilities...");

    let start_instant = Instant::now();
    let mut actions: Vec<Action> = Vec::new();

    // Link-health state. `last_rx` is the last sign of life from either ATVV
    // channel; `probe_sent` is an outstanding keep-alive GET_CAPS awaiting its
    // CAPS_RESP.
    let mut last_rx = Instant::now();
    let mut probe_sent: Option<Instant> = None;
    let mut announced_ready = false;
    let watcher_started = Instant::now();
    let mut ticker = tokio::time::interval(HEALTH_TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let ended = loop {
        tokio::select! {
            _ = shutdown.cancelled() => break Ended::Shutdown,
            // A link drop is reported here and nowhere else: the notification
            // streams below stay silent forever instead of ending or erroring.
            event = next_connection_event(&mut conn_events) => match event {
                Some(ConnectionEvent::Disconnected) => {
                    log::warn!("[BLE STATUS] Remote disconnected. Will re-establish the ATVV session.");
                    break Ended::Disconnected { was_live: session.is_caps_confirmed() };
                }
                Some(ConnectionEvent::Connected) => {
                    // Only reachable if the matching Disconnected was missed;
                    // the subscription is stale either way.
                    log::warn!("[BLE STATUS] Remote reconnected underneath us. Rebuilding the ATVV session.");
                    break Ended::Disconnected { was_live: session.is_caps_confirmed() };
                }
                None => {
                    // In practice this, not a `Disconnected` item, is how a
                    // dropped link shows up: BlueZ tears the device's property
                    // session down and bluer's stream simply ends (measured on
                    // bluer 0.16 / BlueZ 5.x).
                    //
                    // Deliberately *not* confirmed with `is_connected()`: the
                    // remote often re-associates within a second or two, so by
                    // the time we ask, the answer is "connected" again — while
                    // our GATT subscription still belongs to the dead link.
                    // Believing that check is precisely how the gateway used to
                    // go silently mute. The end of the stream means the session
                    // we were watching is gone, so rebuild unconditionally.
                    if watcher_started.elapsed() < WATCHER_MIN_LIFETIME && device.is_connected().await {
                        // Ended immediately: event watching is unavailable here
                        // rather than the link having dropped. Rebuilding on
                        // this would spin, so fall back to the probe instead.
                        log::debug!("[BLE STATUS] Connection event watching unavailable; relying on the keep-alive probe.");
                        conn_events = None;
                    } else {
                        log::warn!("[BLE STATUS] Link session ended (remote dropped off). Rebuilding the ATVV session.");
                        break Ended::Disconnected { was_live: session.is_caps_confirmed() };
                    }
                }
            },
            _ = ticker.tick() => {
                match check_link_health(
                    &device, &tx, tx_props.as_ref(), &session,
                    start_instant, last_rx, &mut probe_sent,
                ).await {
                    HealthCheck::Ok => {}
                    HealthCheck::Broken(reason) => {
                        log::warn!("[BLE ATVV] {reason} Rebuilding the ATVV session.");
                        break Ended::Disconnected { was_live: session.is_caps_confirmed() };
                    }
                }
            },
            item = ctl_stream.next() => match item {
                Some(Ok(data)) => {
                    last_rx = Instant::now();
                    probe_sent = None;
                    let elapsed_ms = start_instant.elapsed().as_millis() as u64;
                    session.handle_control(&data, elapsed_ms, &mut actions);
                }
                Some(Err(e)) => {
                    log::warn!("[BLE ATVV] CTL notification error: {e}");
                    break Ended::Disconnected { was_live: session.is_caps_confirmed() };
                }
                None => {
                    log::warn!("[BLE ATVV] CTL stream closed by remote.");
                    break Ended::Disconnected { was_live: session.is_caps_confirmed() };
                }
            },
            item = audio_stream.next() => match item {
                Some(Ok(data)) => {
                    last_rx = Instant::now();
                    probe_sent = None;
                    let elapsed_ms = start_instant.elapsed().as_millis() as u64;
                    session.handle_audio(&data, elapsed_ms, &mut actions);
                }
                Some(Err(e)) => {
                    log::warn!("[BLE ATVV] AUDIO notification error: {e}");
                    break Ended::Disconnected { was_live: session.is_caps_confirmed() };
                }
                None => {
                    log::warn!("[BLE ATVV] AUDIO stream closed by remote.");
                    break Ended::Disconnected { was_live: session.is_caps_confirmed() };
                }
            }
        }

        // Announced only once the remote has actually answered, so the log line
        // means "the mic button works now" rather than "a packet was sent".
        if !announced_ready && session.is_caps_confirmed() {
            announced_ready = true;
            log::info!("[BLE ATVV] Ready! Press remote Mic button to record voice.");
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
            break Ended::Disconnected { was_live: session.is_caps_confirmed() };
        }
    };

    // Close out any recording in flight, whatever ended the round: a drop in
    // the middle of a voice stream must still flush the WAV and fire the `end`
    // hook rather than leave a half-written file behind.
    session.finish(&mut actions);
    for action in actions.drain(..) {
        if let Action::StreamStopped = action {
            log::info!("[BLE ATVV] Voice streaming session STOPPED (link ended mid-stream).");
            sink.on_stream_stop();
        }
    }

    // Only talk to the remote if the link is still there; on a dropped link
    // these writes just block until they time out.
    if device.is_connected().await {
        if let Some(close_pkt) = session.take_mic_close() {
            let _ = write_tx(&tx, tx_props.as_ref(), &close_pkt).await;
        }
    }

    // Dropping the streams issues StopNotify, which is what lets the next round
    // start a genuinely new subscription.
    drop(ctl_stream);
    drop(audio_stream);

    // Never disconnect a link we merely adopted: it is the user's HID keyboard,
    // and dropping it would break navigation keys on every reconnect round.
    if !adopted_existing_link {
        let _ = adapter.disconnect_device(&device).await;
        log::info!("[BLE STATUS] Disconnected from device.");
    }

    Ok(ended)
}

/// Awaits the next connection event, or parks forever when event watching is
/// unavailable, so this can sit in a `select!` arm unconditionally.
async fn next_connection_event<S>(events: &mut Option<S>) -> Option<ConnectionEvent>
where
    S: futures_lite::Stream<Item = ConnectionEvent> + Unpin,
{
    match events.as_mut() {
        Some(stream) => stream.next().await,
        None => std::future::pending().await,
    }
}

enum HealthCheck {
    Ok,
    Broken(String),
}

/// Decides whether the ATVV session is still usable.
///
/// This exists because neither BlueZ nor bluest reports the failure mode that
/// actually happens in daily use: after the link drops and comes back, the
/// remote's CCCD is not re-written, so it silently stops notifying while
/// everything keeps reporting `Connected = true` and `Notifying = true`. The
/// only reliable test is to ask the remote something and see whether it
/// answers — `GET_CAPS` is idempotent and answered in tens of milliseconds.
async fn check_link_health(
    device: &Device,
    tx: &Characteristic,
    tx_props: Option<&CharacteristicProperties>,
    session: &AtvvSession,
    start: Instant,
    last_rx: Instant,
    probe_sent: &mut Option<Instant>,
) -> HealthCheck {
    // 1. The opening handshake must complete, or the subscription never
    //    became live in the first place.
    if !session.is_caps_confirmed() {
        if start.elapsed() > HANDSHAKE_TIMEOUT {
            return HealthCheck::Broken(format!(
                "No CAPS_RESP within {:.0}s of GET_CAPS; the remote is not notifying.",
                HANDSHAKE_TIMEOUT.as_secs_f64()
            ));
        }
        return HealthCheck::Ok;
    }

    // 2. An outstanding keep-alive must be answered.
    if let Some(sent) = *probe_sent {
        if sent.elapsed() > PROBE_TIMEOUT {
            return HealthCheck::Broken(format!(
                "Keep-alive GET_CAPS unanswered for {:.0}s; the subscription went stale \
                 (BlueZ still claims the link is up).",
                PROBE_TIMEOUT.as_secs_f64()
            ));
        }
        return HealthCheck::Ok;
    }

    // 3. Never probe mid-recording, and never treat plain idleness as failure:
    //    a remote nobody touches is silent for hours by design.
    if session.is_streaming() || last_rx.elapsed() < IDLE_PROBE_INTERVAL {
        return HealthCheck::Ok;
    }

    if !device.is_connected().await {
        return HealthCheck::Broken("Link is down.".into());
    }

    log::debug!("[BLE ATVV] Idle; probing the link with a keep-alive GET_CAPS.");
    if let Err(e) = write_tx(tx, tx_props, &atvv::GET_CAPS).await {
        return HealthCheck::Broken(format!("Keep-alive write failed: {e}."));
    }
    *probe_sent = Some(Instant::now());
    HealthCheck::Ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_then_caps() {
        let config = BridgeConfig {
            reconnect_delay: Duration::from_secs(3),
            ..Default::default()
        };
        assert_eq!(backoff_delay(&config, 0), Duration::from_secs(3));
        assert_eq!(backoff_delay(&config, 1), Duration::from_secs(6));
        assert_eq!(backoff_delay(&config, 3), Duration::from_secs(24));
        // Capped, and stays capped however long the remote stays away.
        assert_eq!(backoff_delay(&config, 5), MAX_RECONNECT_DELAY);
        assert_eq!(backoff_delay(&config, u32::MAX), MAX_RECONNECT_DELAY);
    }

    #[test]
    fn a_live_round_resets_the_backoff() {
        // `was_live` is what the outer loop keys on; a round that reached ATVV
        // must go back to the base delay so flapping links reconnect fast.
        let config = BridgeConfig::default();
        assert_eq!(backoff_delay(&config, 0), config.reconnect_delay);
    }
}
