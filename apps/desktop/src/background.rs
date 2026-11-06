use crate::AppWindow;
use serde::{Deserialize, Serialize};
use slint::ComponentHandle;
use std::{
    fs,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
struct Settings {
    path: String,
    opacity: i32,
    blur: i32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            path: String::new(),
            opacity: 35,
            blur: 8,
        }
    }
}

impl Settings {
    fn normalized(mut self) -> Self {
        self.path = self.path.trim().to_owned();
        self.opacity = self.opacity.clamp(0, 100);
        self.blur = self.blur.clamp(0, 30);
        self
    }
}

fn load() -> Settings {
    crate::project::config_dir()
        .and_then(|dir| fs::read(dir.join("background.json")).ok())
        .and_then(|data| serde_json::from_slice::<Settings>(&data).ok())
        .unwrap_or_default()
        .normalized()
}

fn write_to(dir: &Path, settings: &Settings) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut file = tempfile::NamedTempFile::new_in(dir).map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(&mut file, &settings.clone().normalized())
        .map_err(|e| e.to_string())?;
    file.persist(dir.join("background.json"))
        .map_err(|e| e.error.to_string())?;
    Ok(())
}

fn save(settings: &Settings) -> Result<(), String> {
    let dir = crate::project::config_dir().ok_or("无法确定设置目录")?;
    write_to(&dir, settings)
}

fn render(settings: &Settings) -> Result<(u32, u32, Vec<u8>), String> {
    let mut reader = image::ImageReader::open(&settings.path)
        .map_err(|e| format!("无法打开背景图片：{e}"))?
        .with_guessed_format()
        .map_err(|e| format!("无法识别背景图片：{e}"))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|e| format!("无法读取背景图片：{e}"))?;
    let image = if image.width() > 2560 || image.height() > 2560 {
        image.resize(2560, 2560, image::imageops::FilterType::Triangle)
    } else {
        image
    }
    .to_rgba8();
    let image = if settings.blur > 0 {
        image::imageops::blur(&image, settings.blur as f32)
    } else {
        image
    };
    Ok((image.width(), image.height(), image.into_raw()))
}

fn apply(ui: &AppWindow, settings: Settings, generation: Arc<AtomicU64>, persist: bool) {
    let settings = settings.normalized();
    let id = generation.fetch_add(1, Ordering::SeqCst) + 1;
    if settings.path.is_empty() {
        ui.set_background_loading(false);
        if persist {
            if let Err(error) = save(&settings) {
                ui.set_settings_message(format!("无法保存背景设置：{error}").into());
                return;
            }
        }
        ui.set_background_active(false);
        ui.set_background_image(slint::Image::default());
        ui.invoke_background_changed();
        if persist {
            ui.set_settings_message("已恢复默认背景".into());
        }
        return;
    }
    ui.set_background_loading(true);
    if persist {
        ui.set_settings_message("正在处理背景图片…".into());
    }
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let result = render(&settings);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            if generation.load(Ordering::SeqCst) != id {
                return;
            }
            ui.set_background_loading(false);
            match result {
                Ok((width, height, pixels)) => {
                    if persist {
                        if let Err(error) = save(&settings) {
                            ui.set_settings_message(format!("无法保存背景设置：{error}").into());
                            return;
                        }
                    }
                    let buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                        &pixels, width, height,
                    );
                    ui.set_background_image(slint::Image::from_rgba8(buffer));
                    ui.set_background_opacity(settings.opacity);
                    ui.set_background_active(true);
                    ui.invoke_background_changed();
                    if persist {
                        ui.set_settings_message("背景已应用并保存".into());
                    }
                }
                Err(error) => {
                    if persist {
                        ui.set_settings_message(error.into());
                    } else {
                        ui.set_status(format!("{error}，已使用默认背景").into());
                    }
                }
            }
        });
    });
}

pub fn setup(ui: &AppWindow) {
    let preview_generation = Arc::new(AtomicU64::new(0));
    let preview_timer = std::rc::Rc::new(slint::Timer::default());
    let weak = ui.as_weak();
    ui.on_preview_background(move || {
        let Some(ui) = weak.upgrade() else { return };
        let id = preview_generation.fetch_add(1, Ordering::SeqCst) + 1;
        preview_timer.stop();
        ui.set_background_preview_ready(false);
        if !ui.get_settings_open() || ui.get_settings_tab() != 6 {
            ui.set_background_preview_image(slint::Image::default());
            return;
        }
        let settings = Settings {
            path: ui.get_background_path_draft().to_string(),
            opacity: ui.get_background_opacity_draft(),
            blur: ui.get_background_blur_draft(),
        }
        .normalized();
        if settings.path.is_empty() {
            ui.set_background_preview_message("选择图片以预览".into());
            return;
        }
        ui.set_background_preview_message("正在生成预览…".into());
        let weak = ui.as_weak();
        let generation = preview_generation.clone();
        preview_timer.start(
            slint::TimerMode::SingleShot,
            std::time::Duration::from_millis(300),
            move || {
                let weak = weak.clone();
                let generation = generation.clone();
                let settings = settings.clone();
                std::thread::spawn(move || {
                    let result = render(&settings);
                    let _ = weak.upgrade_in_event_loop(move |ui| {
                        if generation.load(Ordering::SeqCst) != id {
                            return;
                        }
                        match result {
                            Ok((width, height, pixels)) => {
                                let buffer =
                                    slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(
                                        &pixels, width, height,
                                    );
                                ui.set_background_preview_image(slint::Image::from_rgba8(buffer));
                                ui.set_background_preview_ready(true);
                            }
                            Err(error) => ui.set_background_preview_message(error.into()),
                        }
                    });
                });
            },
        );
    });
    let generation = Arc::new(AtomicU64::new(0));
    let settings = load();
    ui.set_background_path_draft(settings.path.as_str().into());
    ui.set_background_opacity_draft(settings.opacity);
    ui.set_background_blur_draft(settings.blur);
    apply(ui, settings, generation.clone(), false);
    let weak = ui.as_weak();
    let apply_generation = generation.clone();
    ui.on_apply_background(move || {
        if let Some(ui) = weak.upgrade() {
            let settings = Settings {
                path: ui.get_background_path_draft().to_string(),
                opacity: ui.get_background_opacity_draft(),
                blur: ui.get_background_blur_draft(),
            };
            if settings.path.trim().is_empty() {
                ui.set_settings_message("请选择背景图片，或点击恢复默认背景".into());
                return;
            }
            apply(&ui, settings, apply_generation.clone(), true);
        }
    });
    let weak = ui.as_weak();
    ui.on_clear_background(move || {
        if let Some(ui) = weak.upgrade() {
            let settings = Settings::default();
            apply(&ui, settings.clone(), generation.clone(), true);
            if !ui.get_background_active() {
                ui.set_background_path_draft("".into());
                ui.set_background_opacity_draft(settings.opacity);
                ui.set_background_blur_draft(settings.blur);
            }
        }
    });
    let weak = ui.as_weak();
    ui.on_browse_background(move || {
        let Some(ui) = weak.upgrade() else { return };
        if ui.get_file_dialog_open() {
            return;
        }
        let dialog = rfd::AsyncFileDialog::new()
            .set_title("选择背景图片")
            .add_filter("图片", &["png", "jpg", "jpeg", "webp", "bmp"])
            .set_parent(&ui.window().window_handle());
        ui.set_file_dialog_open(true);
        let weak = ui.as_weak();
        if let Err(error) = slint::spawn_local(async move {
            let selected = dialog.pick_file().await;
            if let Some(ui) = weak.upgrade() {
                ui.set_file_dialog_open(false);
                if let Some(file) = selected {
                    if let Some(path) = file.path().to_str() {
                        ui.set_background_path_draft(path.into());
                    } else {
                        ui.set_settings_message("所选文件路径包含无法显示的字符".into());
                    }
                }
            }
        }) {
            ui.set_file_dialog_open(false);
            ui.set_settings_message(format!("无法打开文件选择窗口：{error}").into());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn settings_round_trip_and_limits() {
        let dir = tempfile::tempdir().unwrap();
        let settings = Settings {
            path: " test.png ".into(),
            opacity: 120,
            blur: -1,
        };
        write_to(dir.path(), &settings).unwrap();
        let stored: Settings =
            serde_json::from_slice(&fs::read(dir.path().join("background.json")).unwrap()).unwrap();
        assert_eq!(
            stored,
            Settings {
                path: "test.png".into(),
                opacity: 100,
                blur: 0
            }
        );
    }
    #[test]
    fn blur_changes_pixels_and_missing_file_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.png");
        let mut image = image::RgbaImage::new(9, 9);
        image.put_pixel(4, 4, image::Rgba([255, 255, 255, 255]));
        image.save(&path).unwrap();
        let mut settings = Settings {
            path: path.to_string_lossy().into_owned(),
            opacity: 50,
            blur: 0,
        };
        let (_, _, sharp) = render(&settings).unwrap();
        settings.blur = 2;
        let (_, _, blurred) = render(&settings).unwrap();
        assert_ne!(sharp, blurred);
        settings.path = dir
            .path()
            .join("missing.png")
            .to_string_lossy()
            .into_owned();
        assert!(render(&settings).is_err());
    }
}
