use crate::AppWindow;
use nya_domain::{
    firmware::{FirmwareBackend, FirmwareImage},
    Result,
};
use nya_firmware_backend::{NativeFirmwareBackend, ParseProgress};
use slint::ComponentHandle;
use std::{
    cell::Cell,
    path::Path,
    rc::Rc,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone, Default)]
pub struct ProgressBackend(Arc<Mutex<Option<ParseProgress>>>);

impl ProgressBackend {
    pub fn preparing_view(&self) {
        self.report(ParseProgress::new("构建文件树与源码索引", 0.97, 0.0, 0, 0));
    }

    fn report(&self, progress: ParseProgress) {
        *self.0.lock().expect("firmware progress lock") = Some(progress);
    }
}

impl FirmwareBackend for ProgressBackend {
    fn load(&self, path: &Path) -> Result<FirmwareImage> {
        NativeFirmwareBackend.load_with_progress(path, &mut |value| self.report(value))
    }
}

pub fn setup(ui: &AppWindow) -> (ProgressBackend, slint::Timer) {
    let backend = ProgressBackend::default();
    let progress = backend.clone();
    let started = Rc::new(Cell::new(Instant::now()));
    let start_time = started.clone();
    let weak = ui.as_weak();
    ui.on_begin_firmware_parse(move |path| {
        *progress.0.lock().expect("firmware progress lock") = None;
        start_time.set(Instant::now());
        if let Some(ui) = weak.upgrade() {
            ui.set_firmware_parse_path(path);
            ui.set_firmware_parse_stage("准备解析…".into());
            ui.set_firmware_parse_progress(0.0);
            ui.set_firmware_parse_details("已用时 0.0 秒".into());
            ui.set_firmware_parsing(true);
            ui.set_busy(true);
        }
    });
    let timer = slint::Timer::default();
    let weak = ui.as_weak();
    let progress = backend.clone();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(40),
        move || {
            let Some(ui) = weak.upgrade() else { return };
            if !ui.get_firmware_parsing() {
                return;
            }
            let value = progress.0.lock().expect("firmware progress lock").clone();
            let mut details = format!("已用时 {:.1} 秒", started.get().elapsed().as_secs_f64());
            if let Some(value) = value {
                ui.set_firmware_parse_stage(value.stage.into());
                ui.set_firmware_parse_progress(value.fraction);
                if value.total > 0 {
                    details.push_str(&format!(" · {} / {}", value.completed, value.total));
                }
            }
            ui.set_firmware_parse_details(details.into());
        },
    );
    (backend, timer)
}

pub fn finish(ui: &AppWindow) {
    ui.set_firmware_parsing(false);
    ui.set_busy(false);
}
