use crate::{watch_list::SavedWatch, AppWindow};
use serde::{Deserialize, Serialize};
use slint::Model;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
pub struct Project {
    pub format: u32,
    pub firmware: String,
    pub source: String,
    pub gdb: String,
    pub target: String,
    pub interface: String,
    pub port: String,
    pub external_server: bool,
    pub backend_kind: i32,
    pub backend_version: String,
    pub acquisition_endpoint: String,
    #[serde(default)]
    pub refresh_rate_index: i32,
    pub live_interval_index: i32,
    pub live_custom_hz: String,
    pub watches: Vec<SavedWatch>,
}

impl Project {
    pub fn capture(ui: &AppWindow, watches: Vec<SavedWatch>) -> Self {
        let tools = ui.get_tools();
        let backend_version = (0..tools.row_count())
            .filter_map(|i| tools.row_data(i))
            .find(|row| row.active)
            .map(|row| row.version.to_string())
            .unwrap_or_default();
        Self {
            format: 1,
            firmware: ui.get_firmware_path().to_string(),
            source: ui.get_source_path().to_string(),
            gdb: ui.get_gdb_path().to_string(),
            target: ui.get_debug_target().to_string(),
            interface: ui.get_debug_interface().to_string(),
            port: ui.get_debug_port().to_string(),
            external_server: ui.get_external_server(),
            backend_kind: ui.get_backend_kind(),
            backend_version,
            acquisition_endpoint: ui.get_acquisition_endpoint().to_string(),
            refresh_rate_index: ui.get_refresh_rate_index(),
            live_interval_index: ui.get_live_interval_index(),
            live_custom_hz: ui.get_live_custom_hz().to_string(),
            watches,
        }
    }

    pub fn read(path: &Path) -> Result<Self, String> {
        if fs::metadata(path).map_err(|e| e.to_string())?.len() > 4 * 1024 * 1024 {
            return Err("项目文件超过 4 MiB".into());
        }
        let data = fs::read(path).map_err(|e| e.to_string())?;
        if data.len() > 4 * 1024 * 1024 {
            return Err("项目文件超过 4 MiB".into());
        }
        let project: Self = serde_json::from_slice(&data).map_err(|e| e.to_string())?;
        if project.format != 1 {
            return Err("不支持的项目文件版本".into());
        }
        Ok(project)
    }

    pub fn write(&self, path: &Path) -> Result<(), String> {
        let data = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        let parent = path.parent().ok_or("无效的项目路径")?;
        let mut tmp = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
        tmp.write_all(&data).map_err(|e| e.to_string())?;
        tmp.persist(path).map_err(|e| e.error.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_round_trip_and_version_validation() {
        let dir = std::env::temp_dir().join(format!("nya-project-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.nyaproj");
        let project = Project {
            format: 1,
            firmware: "firmware.elf".into(),
            source: "".into(),
            gdb: "gdb".into(),
            target: "chip".into(),
            interface: "probe.cfg".into(),
            port: "3333".into(),
            external_server: false,
            backend_kind: 0,
            backend_version: "system".into(),
            acquisition_endpoint: "127.0.0.1:6666".into(),
            refresh_rate_index: 0,
            live_interval_index: 5,
            live_custom_hz: "10".into(),
            watches: vec![SavedWatch {
                expression: "counter".into(),
                scope: Some("main.c:1".into()),
                function: None,
                waveform: true,
                waveform_members: vec![],
            }],
        };
        project.write(&path).unwrap();
        let loaded = Project::read(&path).unwrap();
        assert_eq!(loaded.target, "chip");
        assert_eq!(loaded.refresh_rate_index, 0);
        let mut legacy = serde_json::to_value(&loaded).unwrap();
        legacy.as_object_mut().unwrap().remove("refresh_rate_index");
        let legacy: Project = serde_json::from_value(legacy).unwrap();
        assert_eq!(legacy.refresh_rate_index, 0);
        assert!(loaded.watches[0].waveform);
        assert_eq!(loaded.watches[0].scope.as_deref(), Some("main.c:1"));
        let mut future = loaded;
        future.format = 2;
        future.write(&path).unwrap();
        assert!(Project::read(&path).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}

fn history_path() -> Option<PathBuf> {
    let base = if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|p| PathBuf::from(p).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))
    }?;
    Some(base.join("NyaScraping/projects.json"))
}

pub fn history() -> Vec<PathBuf> {
    history_path()
        .and_then(|path| fs::read(path).ok())
        .and_then(|data| serde_json::from_slice(&data).ok())
        .unwrap_or_default()
}

pub fn remember(path: &Path) -> Result<(), String> {
    let path = fs::canonicalize(path).map_err(|e| e.to_string())?;
    let mut items = history();
    items.retain(|item| item != &path);
    items.insert(0, path);
    items.truncate(30);
    let target = history_path().ok_or("无法确定历史记录目录")?;
    fs::create_dir_all(target.parent().ok_or("无效的历史记录目录")?).map_err(|e| e.to_string())?;
    fs::write(
        target,
        serde_json::to_vec_pretty(&items).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}
