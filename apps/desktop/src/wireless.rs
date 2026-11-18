use crate::{project, submit, AppWindow};
use nya_application::Command;
use nya_domain::WirelessConfig;
use serde::{Deserialize, Serialize};
use slint::{ComponentHandle, Model};
use std::{
    io::Write,
    path::Path,
    rc::Rc,
    sync::mpsc::{self, SyncSender},
    time::Duration,
};

/// Preferences contain public device IDs and connection parameters, never credentials.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    pub device_id: String,
    pub endpoint: String,
    pub computer_name: String,
    pub target: String,
    pub core: String,
    pub speed: String,
    pub protocol_index: i32,
    pub under_reset: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            device_id: String::new(),
            endpoint: String::new(),
            computer_name: "NyaScraping".into(),
            target: String::new(),
            core: "0".into(),
            speed: "1000".into(),
            protocol_index: 0,
            under_reset: false,
        }
    }
}
impl Settings {
    pub fn capture(ui: &AppWindow) -> Self {
        Self {
            enabled: ui.get_wireless_enabled(),
            device_id: ui.get_wireless_device_id().to_string(),
            endpoint: ui.get_wireless_endpoint().to_string(),
            computer_name: ui.get_wireless_computer_name().to_string(),
            target: ui.get_wireless_target().to_string(),
            core: ui.get_wireless_core().to_string(),
            speed: ui.get_wireless_speed().to_string(),
            protocol_index: ui.get_wireless_protocol_index(),
            under_reset: ui.get_wireless_under_reset(),
        }
    }
    pub fn apply(&self, ui: &AppWindow) {
        ui.set_wireless_enabled(self.enabled);
        ui.set_wireless_device_id(self.device_id.as_str().into());
        ui.set_wireless_endpoint(self.endpoint.as_str().into());
        ui.set_wireless_computer_name(self.computer_name.as_str().into());
        ui.set_wireless_target(self.target.as_str().into());
        ui.set_wireless_core(self.core.as_str().into());
        ui.set_wireless_speed(self.speed.as_str().into());
        ui.set_wireless_protocol_index(self.protocol_index);
        ui.set_wireless_under_reset(self.under_reset);
        sync_selection(ui);
    }
    fn connection(&self, standalone_flash: bool) -> Result<WirelessConfig, String> {
        if self.device_id.is_empty() {
            return Err("请先配对并选择手机".into());
        }
        let core = self
            .core
            .trim()
            .parse::<u32>()
            .map_err(|_| "核心编号应为非负整数")?;
        if standalone_flash && core != 0 {
            return Err("独立烧录仅支持核心 0；其他核心请连接后下载".into());
        }
        let speed_khz = self
            .speed
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|v| (1..=50000).contains(v))
            .ok_or("无线调试速率应为 1–50000 kHz")?;
        let protocol = match self.protocol_index {
            0 => "swd",
            1 => "jtag",
            _ => return Err("请选择 SWD 或 JTAG".into()),
        };
        Ok(WirelessConfig {
            device_id: self.device_id.clone(),
            endpoint: (!self.endpoint.trim().is_empty()).then(|| self.endpoint.trim().into()),
            core,
            speed_khz,
            protocol: protocol.into(),
            under_reset: self.under_reset,
        })
    }
}

pub fn connection(ui: &AppWindow, standalone_flash: bool) -> Result<WirelessConfig, String> {
    let settings = Settings::capture(ui);
    if !ui
        .get_wireless_devices()
        .iter()
        .any(|device| device.id.as_str() == settings.device_id)
    {
        return Err("当前电脑尚未配对所选手机，请先在调试器设置中配对".into());
    }
    settings.connection(standalone_flash)
}

fn save(path: &Path, settings: &Settings) -> Result<(), String> {
    let directory = path.parent().ok_or("无效的无线设置路径")?;
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let mut temporary = tempfile::NamedTempFile::new_in(directory).map_err(|e| e.to_string())?;
    temporary
        .write_all(&serde_json::to_vec_pretty(settings).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    temporary.persist(path).map_err(|e| e.error.to_string())?;
    Ok(())
}

pub fn save_settings(ui: &AppWindow) -> Result<(), String> {
    let path = project::config_dir()
        .ok_or("无法确定桌面配置目录")?
        .join("wireless.json");
    save(&path, &Settings::capture(ui))
}

fn sync_selection(ui: &AppWindow) {
    let selected = ui
        .get_wireless_devices()
        .iter()
        .position(|device| device.id == ui.get_wireless_device_id());
    ui.set_wireless_device_index(selected.map(|i| i as i32).unwrap_or(-1));
}

/// Timers remain alive with the main window; all network operations use its worker.
pub fn setup(ui: &AppWindow, sender: &SyncSender<Command>) -> (Rc<slint::Timer>, slint::Timer) {
    let path = project::config_dir().map(|dir| dir.join("wireless.json"));
    if let Some(path) = &path {
        match std::fs::read(path) {
            Ok(bytes) => match serde_json::from_slice::<Settings>(&bytes) {
                Ok(settings) => settings.apply(ui),
                Err(e) => ui.set_wireless_message(format!("无法读取无线设置：{e}").into()),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => ui.set_wireless_message(format!("无法读取无线设置：{e}").into()),
        }
    }
    let save_timer = Rc::new(slint::Timer::default());
    let timer = save_timer.clone();
    let weak = ui.as_weak();
    ui.on_wireless_settings_changed(move || {
        let weak = weak.clone();
        let path = path.clone();
        timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(350),
            move || {
                if let (Some(ui), Some(path)) = (weak.upgrade(), &path) {
                    if let Err(error) = save(path, &Settings::capture(&ui)) {
                        ui.set_wireless_message(format!("无法保存无线设置：{error}").into());
                    }
                }
            },
        );
    });
    let (code_sender, code_receiver) = mpsc::sync_channel::<String>(1);
    let code_timer = slint::Timer::default();
    let weak = ui.as_weak();
    code_timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(50),
        move || {
            if let Some(ui) = weak.upgrade() {
                while let Ok(code) = code_receiver.try_recv() {
                    if ui.get_wireless_pairing() {
                        ui.set_wireless_comparison_code(code.into());
                    }
                }
            }
        },
    );
    let weak = ui.as_weak();
    ui.on_select_wireless_device(move |index| {
        if let Some(ui) = weak.upgrade() {
            if ui.get_busy() || ui.get_debug_connected() {
                return;
            }
            if let Some(device) = usize::try_from(index)
                .ok()
                .and_then(|i| ui.get_wireless_devices().row_data(i))
            {
                ui.set_wireless_device_id(device.id);
                ui.set_wireless_endpoint(device.endpoint);
                sync_selection(&ui);
            }
        }
    });
    let weak = ui.as_weak();
    let commands = sender.clone();
    ui.on_refresh_wireless(move || {
        if let Some(ui) = weak.upgrade() {
            if !ui.get_debug_connected() {
                submit(&ui, &commands, Command::RefreshWirelessDevices);
            }
        }
    });
    let weak = ui.as_weak();
    let commands = sender.clone();
    ui.on_pair_wireless(move || {
        if let Some(ui) = weak.upgrade() {
            if ui.get_busy() || ui.get_debug_connected() || ui.get_file_dialog_open() {
                return;
            }
            ui.set_wireless_comparison_code("".into());
            ui.set_wireless_message("正在连接手机，请等待配对校验码…".into());
            ui.set_wireless_pairing(true);
            submit(
                &ui,
                &commands,
                Command::PairWireless {
                    endpoint: ui.get_wireless_endpoint().trim().into(),
                    computer_name: ui.get_wireless_computer_name().trim().into(),
                    comparison: code_sender.clone(),
                },
            );
            if !ui.get_busy() {
                ui.set_wireless_pairing(false);
            }
        }
    });
    let weak = ui.as_weak();
    let commands = sender.clone();
    ui.on_forget_wireless(move || {
        if let Some(ui) = weak.upgrade() {
            if !ui.get_debug_connected() && ui.get_wireless_device_index() >= 0 {
                submit(
                    &ui,
                    &commands,
                    Command::ForgetWireless(ui.get_wireless_device_id().to_string()),
                );
            }
        }
    });
    (save_timer, code_timer)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wireless_preferences_preserve_public_parameters_and_validate_before_submit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wireless.json");
        let mut settings = Settings {
            enabled: true,
            device_id: "phone".into(),
            endpoint: "192.168.1.2:4242".into(),
            target: "STM32H723ZGTx".into(),
            protocol_index: 1,
            under_reset: true,
            ..Default::default()
        };
        save(&path, &settings).unwrap();
        let loaded: Settings = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(loaded, settings);
        let config = loaded.connection(true).unwrap();
        assert_eq!(config.protocol, "jtag");
        assert_eq!(config.speed_khz, 1000);
        settings.core = "1".into();
        assert!(settings.connection(true).is_err());
        assert_eq!(settings.connection(false).unwrap().core, 1);
        for speed in ["0", "50001", "abc"] {
            settings.speed = speed.into();
            assert!(settings.connection(false).is_err());
        }
        assert!(!serde_json::to_string(&loaded).unwrap().contains("token"));
        let legacy: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(legacy, Settings::default());
    }
}
