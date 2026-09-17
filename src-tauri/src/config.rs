//! Persistent settings in %APPDATA%\Smowaudio\config.json.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::audio::defaults::WindowsDefaults;
use crate::audio::device::{self, DeviceInfo, Flow};
use crate::dsp::chain::{ChannelSettings, MicSettings};

/// Same channels as SteelSeries Sonar. Game is meant to be the Windows default output, so it
/// also carries system sounds and anything not assigned elsewhere.
pub const CHANNEL_NAMES: [&str; 4] = ["Game", "Chat", "Media", "Aux"];
pub const CHANNEL_COUNT: usize = CHANNEL_NAMES.len();

/// Cable each channel gets when it has none. Cables C and D run natively at 48 kHz with the
/// smallest buffer, so they go to the most delay-sensitive paths: Game and the Virtual Mic.
const PREFERRED_CABLES: [&str; CHANNEL_COUNT] =
    ["VB-Audio Cable D", "VB-Audio Cable A", "VB-Audio Cable B", "VB-Audio Virtual Cable"];
const MIC_CABLE: &str = "VB-Audio Cable C";

/// Version of the channel layout. 0/1: Game, Media, System (+ Chat). 2: Game, Chat, Media, Aux.
const LAYOUT: u32 = 2;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ChannelConfig {
    /// Capture side ("CABLE-A Output") of the virtual cable apps play into.
    pub source: Option<String>,
    /// Render side ("CABLE-A Input") that apps get assigned to.
    pub sink: Option<String>,
    pub settings: ChannelSettings,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Missing in configs from before layouts were versioned, which then read as 0.
    #[serde(default)]
    pub layout: u32,
    /// Physical headphones/speakers. None = Windows default (never a virtual cable).
    pub output_device: Option<String>,
    /// Physical microphone. None = Windows default.
    pub mic_device: Option<String>,
    /// Render side of the cable whose capture side apps use as "Virtual Mic".
    pub mic_sink: Option<String>,
    /// One entry per `CHANNEL_NAMES`, in order.
    pub channels: Vec<ChannelConfig>,
    /// Master volume and mute for the final mix.
    pub master: ChannelSettings,
    pub mic: MicSettings,
    /// Lower-case exe name -> channel index.
    pub app_rules: BTreeMap<String, usize>,
    pub launch_at_login: bool,
    /// Like Sonar: make Game / Chat / Virtual Mic the Windows default devices.
    pub set_windows_defaults: bool,
    /// The user's own defaults from before Smowaudio changed them, restored when turned off.
    pub previous_defaults: Option<WindowsDefaults>,
    /// Global keyboard shortcuts: action id (see hotkeys.rs) -> key combination such as
    /// "Ctrl+Alt+KeyM". Empty by default; the user binds them in Settings.
    pub hotkeys: BTreeMap<String, String>,
    /// How much the volume up/down shortcuts change a channel (0.05 = 5 %).
    pub volume_step: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            layout: LAYOUT,
            output_device: None,
            mic_device: None,
            mic_sink: None,
            channels: vec![ChannelConfig::default(); CHANNEL_COUNT],
            master: ChannelSettings::default(),
            mic: MicSettings::default(),
            app_rules: BTreeMap::new(),
            launch_at_login: false,
            set_windows_defaults: true,
            previous_defaults: None,
            hotkeys: BTreeMap::new(),
            volume_step: 0.05,
        }
    }
}

impl Config {
    fn path() -> PathBuf {
        let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
        let path = base.join("Smowaudio").join("config.json");
        // The app used to be called AudioManager: carry its settings over once.
        let legacy = base.join("AudioManager").join("config.json");
        if !path.exists() && legacy.exists() {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::copy(&legacy, &path);
        }
        path
    }

    pub fn load() -> Self {
        let mut config: Config = std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        let migrated = config.layout < LAYOUT;
        if migrated {
            config.migrate_to_sonar_layout();
        }
        config.channels.resize_with(CHANNEL_COUNT, ChannelConfig::default);
        config.auto_assign_cables();
        if migrated {
            crate::append_log(&format!("config: switched to the Game/Chat/Media/Aux layout: {}", config.describe()));
            if let Err(e) = config.save() {
                crate::append_log(&format!("config: saving the new layout failed: {e:#}"));
            }
        }
        config
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        std::fs::create_dir_all(path.parent().unwrap())?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    /// One-line summary for the log: the config file and the device behind each channel.
    pub fn describe(&self) -> String {
        let name = |id: &Option<String>| match id {
            Some(id) => device::by_id(id)
                .and_then(|d| device::friendly_name(&d))
                .unwrap_or_else(|_| format!("missing device {id}")),
            None => "none".to_string(),
        };
        let channels: Vec<String> =
            CHANNEL_NAMES.iter().zip(&self.channels).map(|(n, c)| format!("{n}={}", name(&c.source))).collect();
        format!("{} [{}; mic sink={}]", Self::path().display(), channels.join(", "), name(&self.mic_sink))
    }

    /// The Windows defaults Sonar would set: Game (playback), Chat (communications playback),
    /// Virtual Mic (recording, both roles).
    pub fn windows_default_targets(&self) -> WindowsDefaults {
        let sink = |i: usize| self.channels.get(i).and_then(|c| c.sink.clone());
        let virtual_mic = self.mic_sink.as_deref().and_then(cable_partner);
        WindowsDefaults {
            playback: sink(0),
            playback_communications: sink(1),
            recording: virtual_mic.clone(),
            recording_communications: virtual_mic,
        }
    }

    /// The user's own Windows default from before Smowaudio made the cables default. Used as
    /// the physical device when none is picked explicitly, since the Windows default is now a cable.
    pub fn previous_default(&self, flow: Flow) -> Option<String> {
        let saved = self.previous_defaults.as_ref()?;
        match flow {
            Flow::Render => saved.playback.clone(),
            Flow::Capture => saved.recording.clone(),
        }
    }

    /// The physical devices and cables the engine would use right now. When this changes (e.g.
    /// headphones plugged in while following the Windows default), streams need a restart.
    pub fn device_fingerprint(&self) -> String {
        let active: HashSet<String> = [Flow::Render, Flow::Capture]
            .into_iter()
            .filter_map(|f| device::list(f).ok())
            .flatten()
            .map(|d| d.id)
            .collect();
        let resolve = |flow: Flow, id: &Option<String>| {
            device::resolve_physical(flow, id.as_deref(), self.previous_default(flow).as_deref())
                .ok()
                .and_then(|d| device::device_id(&d).ok())
                .unwrap_or_else(|| "none".into())
        };
        let cables: Vec<&str> = self
            .channels
            .iter()
            .map(|c| &c.source)
            .chain(std::iter::once(&self.mic_sink))
            .map(|id| match id {
                Some(id) if active.contains(id) => "ok",
                Some(_) => "missing",
                None => "-",
            })
            .collect();
        format!(
            "output={} mic={} cables={}",
            resolve(Flow::Render, &self.output_device),
            resolve(Flow::Capture, &self.mic_device),
            cables.join(",")
        )
    }

    /// Exe name -> render device id, for the routing watcher.
    pub fn routing_rules(&self) -> BTreeMap<String, String> {
        self.app_rules
            .iter()
            .filter_map(|(exe, &ch)| Some((exe.clone(), self.channels.get(ch)?.sink.clone()?)))
            .collect()
    }

    /// Converts an older Game, Media, System[, Chat] config to Game, Chat, Media, Aux.
    /// Channel settings and app rules move with their channel; System's apps join Game (which is
    /// now the Windows default, like Sonar's Gaming device). Cables are cleared so the new
    /// mapping is applied.
    fn migrate_to_sonar_layout(&mut self) {
        let old = std::mem::take(&mut self.channels);
        let settings = |i: usize| old.get(i).map(|c| c.settings.clone()).unwrap_or_default();
        self.channels = vec![
            ChannelConfig { settings: settings(0), ..Default::default() }, // Game
            ChannelConfig { settings: settings(3), ..Default::default() }, // Chat
            ChannelConfig { settings: settings(1), ..Default::default() }, // Media
            ChannelConfig::default(),                                     // Aux
        ];
        for channel in self.app_rules.values_mut() {
            *channel = match *channel {
                1 => 2, // Media
                3 => 1, // Chat
                _ => 0, // Game, System
            };
        }
        self.mic_sink = None;
        self.layout = LAYOUT;
    }

    /// Gives every channel (and the Virtual Mic) that has no cable yet its preferred VB-Audio
    /// cable, if installed and not already used elsewhere. Existing assignments never change,
    /// so installing another cable later just fills the channel still missing one.
    fn auto_assign_cables(&mut self) {
        let (Ok(renders), Ok(captures)) = (device::list(Flow::Render), device::list(Flow::Capture)) else {
            return;
        };
        let hardware_of =
            |id: &str| renders.iter().chain(&captures).find(|d| d.id == id).map(|d| d.hardware.to_lowercase());
        let mut used: HashSet<String> = self
            .channels
            .iter()
            .filter_map(|c| c.source.as_deref())
            .chain(self.mic_sink.as_deref())
            .filter_map(hardware_of)
            .collect();

        if self.mic_sink.is_none() && !used.contains(&MIC_CABLE.to_lowercase()) {
            if let Some(sink) = pick(&renders, MIC_CABLE) {
                self.mic_sink = Some(sink.id.clone());
                used.insert(MIC_CABLE.to_lowercase());
            }
        }
        for (channel, hardware) in self.channels.iter_mut().zip(PREFERRED_CABLES) {
            if channel.source.is_some() || used.contains(&hardware.to_lowercase()) {
                continue;
            }
            if let (Some(source), Some(sink)) = (pick(&captures, hardware), pick(&renders, hardware)) {
                channel.source = Some(source.id.clone());
                channel.sink = Some(sink.id.clone());
                used.insert(hardware.to_lowercase());
            }
        }
    }
}

/// The endpoint to use for a cable. VB-CABLE also exposes a 16-channel "CABLE In 16ch" input
/// next to "CABLE Input", so prefer the regular stereo endpoint.
fn pick<'a>(list: &'a [DeviceInfo], hardware: &str) -> Option<&'a DeviceInfo> {
    list.iter().filter(|d| d.hardware.eq_ignore_ascii_case(hardware)).min_by_key(|d| d.name.contains("16ch"))
}

/// Finds the other end of a virtual cable (render <-> capture) by its hardware name.
pub fn cable_partner(id: &str) -> Option<String> {
    let all: Vec<DeviceInfo> =
        [Flow::Render, Flow::Capture].into_iter().filter_map(|f| device::list(f).ok()).flatten().collect();
    let me = all.iter().find(|d| d.id == id)?;
    let others: Vec<DeviceInfo> = all.iter().filter(|d| d.flow != me.flow).cloned().collect();
    pick(&others, &me.hardware).map(|d| d.id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel_with_volume(source: &str, volume: f32) -> ChannelConfig {
        let mut c = ChannelConfig { source: Some(source.into()), sink: Some(format!("{source}-sink")), ..Default::default() };
        c.settings.volume = volume;
        c
    }

    #[test]
    fn old_layout_moves_settings_and_rules_to_sonar_channels() {
        let mut config = Config {
            layout: 0,
            channels: vec![
                channel_with_volume("a", 0.1), // Game
                channel_with_volume("b", 0.2), // Media
                channel_with_volume("d", 0.3), // System
                channel_with_volume("v", 0.4), // Chat
            ],
            mic_sink: Some("c".into()),
            app_rules: [("chrome.exe", 1), ("discord.exe", 3), ("explorer.exe", 2), ("cs2.exe", 0)]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            ..Default::default()
        };
        config.migrate_to_sonar_layout();

        let volumes: Vec<f32> = config.channels.iter().map(|c| c.settings.volume).collect();
        assert_eq!(volumes, vec![0.1, 0.4, 0.2, 1.0], "Game, Chat, Media keep their settings; Aux is new");
        assert!(config.channels.iter().all(|c| c.source.is_none() && c.sink.is_none()), "cables get re-assigned");
        assert_eq!(config.mic_sink, None);
        assert_eq!(config.app_rules["chrome.exe"], 2);
        assert_eq!(config.app_rules["discord.exe"], 1);
        assert_eq!(config.app_rules["explorer.exe"], 0, "System apps join Game");
        assert_eq!(config.app_rules["cs2.exe"], 0);
        assert_eq!(config.layout, LAYOUT);
    }

    #[test]
    fn three_channel_config_migrates_too() {
        let mut config = Config {
            layout: 0,
            channels: vec![channel_with_volume("a", 0.5), channel_with_volume("b", 0.6), channel_with_volume("d", 0.7)],
            ..Default::default()
        };
        config.migrate_to_sonar_layout();
        let volumes: Vec<f32> = config.channels.iter().map(|c| c.settings.volume).collect();
        assert_eq!(volumes, vec![0.5, 1.0, 0.6, 1.0]);
    }

    #[test]
    fn layout_version_distinguishes_old_and_new_files() {
        let old: Config = serde_json::from_str(r#"{"channels": []}"#).unwrap();
        assert_eq!(old.layout, 0, "files without a layout field are old");
        assert_eq!(Config::default().layout, LAYOUT, "fresh configs start on the current layout");
    }
}
