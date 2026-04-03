//! WiFi network scanning: active and passive scan, SSID filtering,
//! signal strength tracking.

use alloc::string::String;
use alloc::vec::Vec;

/// A discovered WiFi network from a scan.
#[derive(Debug, Clone)]
pub struct ScannedNetwork {
    /// SSID (network name). Empty if hidden.
    pub ssid: String,
    /// BSSID (AP MAC address).
    pub bssid: [u8; 6],
    /// Channel number.
    pub channel: u8,
    /// Signal strength in dBm (negative, higher is better; e.g., -40 is excellent).
    pub signal_dbm: i8,
    /// Whether the network uses encryption (WPA2, WPA3, WEP, etc.).
    pub encrypted: bool,
    /// Whether WPA2 (RSN) was detected.
    pub wpa2: bool,
    /// Beacon interval in TUs.
    pub beacon_interval: u16,
}

impl ScannedNetwork {
    /// Human-readable signal quality description.
    pub fn signal_quality(&self) -> &'static str {
        match self.signal_dbm {
            -30..=0 => "Excellent",
            -50..=-31 => "Very Good",
            -60..=-51 => "Good",
            -70..=-61 => "Fair",
            _ => "Weak",
        }
    }
}

/// Scan configuration.
#[derive(Debug, Clone)]
pub struct ScanConfig {
    /// Channels to scan. If empty, scan all common channels.
    pub channels: Vec<u8>,
    /// SSID to scan for (active scan). If `None`, do a passive scan.
    pub ssid: Option<String>,
    /// Maximum time to wait for scan results, in milliseconds.
    pub timeout_ms: u32,
    /// Minimum signal strength to report (in dBm). Networks weaker than this
    /// are filtered out.
    pub min_signal_dbm: i8,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            channels: Vec::new(),
            ssid: None,
            timeout_ms: 5000,
            min_signal_dbm: -90,
        }
    }
}

impl ScanConfig {
    /// Get the channel list to scan, defaulting to standard 2.4 GHz + 5 GHz channels.
    pub fn channels_to_scan(&self) -> Vec<u8> {
        if !self.channels.is_empty() {
            return self.channels.clone();
        }

        let mut channels = Vec::with_capacity(25);

        // 2.4 GHz channels 1-14
        for ch in 1..=14u8 {
            channels.push(ch);
        }

        // 5 GHz UNII-1 channels
        for &ch in &[36u8, 40, 44, 48] {
            channels.push(ch);
        }

        // 5 GHz UNII-2 channels
        for &ch in &[52u8, 56, 60, 64] {
            channels.push(ch);
        }

        // 5 GHz UNII-3 channels
        for &ch in &[149u8, 153, 157, 161, 165] {
            channels.push(ch);
        }

        channels
    }
}

/// Scan result accumulator. Deduplicates by BSSID, keeping the strongest signal.
pub struct ScanResults {
    networks: Vec<ScannedNetwork>,
}

impl ScanResults {
    /// Create a new empty result set.
    pub fn new() -> Self {
        Self {
            networks: Vec::new(),
        }
    }

    /// Add or update a network in the results.
    ///
    /// If a network with the same BSSID already exists, update it only if
    /// the new signal strength is stronger.
    pub fn add(&mut self, network: ScannedNetwork) {
        for existing in &mut self.networks {
            if existing.bssid == network.bssid {
                if network.signal_dbm > existing.signal_dbm {
                    log::trace!(
                        "wifi::scan: updating BSSID {:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X} signal {} -> {} dBm",
                        network.bssid[0], network.bssid[1], network.bssid[2],
                        network.bssid[3], network.bssid[4], network.bssid[5],
                        existing.signal_dbm, network.signal_dbm
                    );
                    *existing = network;
                }
                return;
            }
        }

        log::debug!(
            "wifi::scan: discovered \"{}\" on ch{} signal={}dBm BSSID={:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
            network.ssid, network.channel, network.signal_dbm,
            network.bssid[0], network.bssid[1], network.bssid[2],
            network.bssid[3], network.bssid[4], network.bssid[5],
        );

        self.networks.push(network);
    }

    /// Process a beacon/probe response and extract network info.
    pub fn process_beacon(&mut self, beacon: &crate::ieee80211::BeaconInfo, signal_dbm: i8, min_signal: i8) {
        if signal_dbm < min_signal {
            return;
        }

        self.add(ScannedNetwork {
            ssid: beacon.ssid.clone(),
            bssid: beacon.bssid,
            channel: beacon.channel,
            signal_dbm,
            encrypted: !beacon.is_open,
            wpa2: beacon.has_rsn,
            beacon_interval: beacon.beacon_interval,
        });
    }

    /// Get the final sorted list of networks (strongest signal first).
    pub fn finalize(mut self) -> Vec<ScannedNetwork> {
        self.networks.sort_by(|a, b| b.signal_dbm.cmp(&a.signal_dbm));

        log::info!("wifi::scan: scan complete — {} networks found", self.networks.len());
        for (i, net) in self.networks.iter().enumerate() {
            log::info!(
                "wifi::scan:   #{}: \"{}\" ch{} {}dBm {} {}",
                i + 1,
                net.ssid,
                net.channel,
                net.signal_dbm,
                net.signal_quality(),
                if net.wpa2 { "WPA2" } else if net.encrypted { "encrypted" } else { "OPEN" }
            );
        }

        self.networks
    }

    /// Number of networks discovered so far.
    pub fn count(&self) -> usize {
        self.networks.len()
    }
}
