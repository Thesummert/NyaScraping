use crate::{submit, AppWindow};
use nya_application::Command;
use slint::ComponentHandle;
use std::{
    collections::hash_map::DefaultHasher,
    fs::File,
    hash::Hasher,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::SyncSender,
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Fingerprint {
    size: u64,
    hash: u64,
}

fn fingerprint(path: &Path) -> Option<Fingerprint> {
    let mut file = File::open(path).ok()?;
    let before = file.metadata().ok()?;
    if !before.is_file() || before.len() == 0 {
        return None;
    }
    let mut hash = DefaultHasher::new();
    let mut buffer = [0; 64 * 1024];
    let mut size = 0;
    loop {
        let count = file.read(&mut buffer).ok()?;
        if count == 0 {
            break;
        }
        hash.write(&buffer[..count]);
        size += count as u64;
    }
    let after = file.metadata().ok()?;
    let current = path.metadata().ok()?;
    if size != before.len()
        || size != after.len()
        || size != current.len()
        || before.modified().ok()? != after.modified().ok()?
        || after.modified().ok()? != current.modified().ok()?
    {
        return None;
    }
    Some(Fingerprint {
        size,
        hash: hash.finish(),
    })
}

#[derive(Default)]
struct Detector {
    path: PathBuf,
    baseline: Option<Fingerprint>,
    candidate: Option<Fingerprint>,
}

impl Detector {
    fn observe(&mut self, path: &Path, sample: Option<Fingerprint>) -> bool {
        if self.path != path {
            *self = Self {
                path: path.into(),
                baseline: sample,
                candidate: None,
            };
            return false;
        }
        let Some(sample) = sample else {
            // Linkers can remove or truncate the output before replacing it.
            self.candidate = None;
            return false;
        };
        if self.baseline.is_none() {
            self.baseline = Some(sample);
            return false;
        }
        if self.baseline == Some(sample) {
            self.candidate = None;
            return false;
        }
        if self.candidate != Some(sample) {
            self.candidate = Some(sample);
            return false;
        }
        self.baseline = Some(sample);
        self.candidate = None;
        true
    }
}

#[derive(Default)]
struct Shared {
    path: PathBuf,
    sample: Option<Fingerprint>,
    pending: Option<Fingerprint>,
}

pub struct FirmwareWatch {
    timer: slint::Timer,
    shutdown: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Drop for FirmwareWatch {
    fn drop(&mut self) {
        self.timer.stop();
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
    }
}

fn check_for_update(ui: &AppWindow, shared: &Mutex<Shared>, sender: &SyncSender<Command>) {
    let path = PathBuf::from(ui.get_firmware_path().trim());
    let mut state = shared.lock().expect("firmware watcher lock");
    if state.path != path {
        *state = Shared {
            path: path.clone(),
            ..Shared::default()
        };
        ui.set_firmware_update_open(false);
    }
    // A later build can replace or truncate the file while the UI is busy.
    if state.sample != state.pending
        || ui.get_busy()
        || ui.get_file_dialog_open()
        || ui.get_firmware_parsing()
        || ui.get_project_dialog_open()
        || ui.get_settings_open()
    {
        return;
    }
    let Some(revision) = state.pending.take() else {
        return;
    };
    drop(state);
    if submit(ui, sender, Command::ReloadFirmware(path.clone())) {
        ui.set_firmware_update_path(path.to_string_lossy().as_ref().into());
        ui.set_firmware_update_open(true);
    } else {
        let mut state = shared.lock().expect("firmware watcher lock");
        state.pending.get_or_insert(revision);
    }
}

pub fn setup(ui: &AppWindow, sender: SyncSender<Command>) -> FirmwareWatch {
    let weak = ui.as_weak();
    ui.on_respond_firmware_update(move |download| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        if !ui.get_firmware_update_open() || ui.get_busy() || ui.get_firmware_parsing() {
            return;
        }
        let path = ui.get_firmware_update_path();
        ui.set_firmware_update_open(false);
        if download && !ui.get_file_dialog_open() && ui.get_firmware_path().trim() == path.as_str()
        {
            ui.invoke_debug_action(8);
        }
    });
    let shared = Arc::new(Mutex::new(Shared::default()));
    let shutdown = Arc::new(AtomicBool::new(false));
    let state = shared.clone();
    let stopping = shutdown.clone();
    let worker = thread::spawn(move || {
        let mut detector = Detector::default();
        while !stopping.load(Ordering::Relaxed) {
            let path = state.lock().expect("firmware watcher lock").path.clone();
            let sample = fingerprint(&path);
            let changed = detector.observe(&path, sample);
            {
                let mut state = state.lock().expect("firmware watcher lock");
                if state.path == path {
                    state.sample = sample;
                    if changed {
                        state.pending = sample;
                    }
                }
            }
            thread::park_timeout(Duration::from_millis(500));
        }
    });
    let timer = slint::Timer::default();
    let weak = ui.as_weak();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(200),
        move || {
            if let Some(ui) = weak.upgrade() {
                check_for_update(&ui, &shared, &sender);
            }
        },
    );
    FirmwareWatch {
        timer,
        shutdown,
        worker: Some(worker),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nya_application::Event;
    use nya_domain::firmware::FirmwareBackend;
    use slint::platform::{
        software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
        Platform, PlatformError, WindowAdapter,
    };
    use std::{rc::Rc, sync::mpsc};

    fn sample(hash: u64) -> Option<Fingerprint> {
        Some(Fingerprint { size: 4, hash })
    }

    #[test]
    fn waits_for_stable_content_and_notifies_once_per_revision() {
        let mut detector = Detector::default();
        let path = Path::new("firmware.elf");
        assert!(!detector.observe(path, sample(1)));
        assert!(!detector.observe(path, sample(2)));
        assert!(!detector.observe(path, sample(3)));
        assert!(detector.observe(path, sample(3)));
        assert!(!detector.observe(path, sample(3)));
        assert!(!detector.observe(path, sample(4)));
        assert!(detector.observe(path, sample(4)));
    }

    #[test]
    fn tolerates_rebuild_gaps_and_resets_when_switching_files() {
        let mut detector = Detector::default();
        let path = Path::new("firmware.elf");
        assert!(!detector.observe(path, sample(1)));
        assert!(!detector.observe(path, None));
        assert!(!detector.observe(path, sample(2)));
        assert!(!detector.observe(path, None));
        assert!(!detector.observe(path, sample(2)));
        assert!(detector.observe(path, sample(2)));
        assert!(!detector.observe(Path::new("other.elf"), sample(3)));
        assert!(!detector.observe(Path::new("other.elf"), sample(3)));
    }

    #[test]
    fn fingerprints_detect_same_size_edits_and_atomic_replacement() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("firmware.elf");
        std::fs::write(&path, b"old!").unwrap();
        let old = fingerprint(&path).unwrap();
        std::fs::write(&path, b"new!").unwrap();
        let new = fingerprint(&path).unwrap();
        assert_ne!(old, new);
        let replacement = root.path().join("replacement");
        std::fs::write(&replacement, b"next").unwrap();
        std::fs::rename(replacement, &path).unwrap();
        assert_ne!(Some(new), fingerprint(&path));
        std::fs::write(&path, b"").unwrap();
        assert_eq!(fingerprint(&path), None);
    }

    #[test]
    fn pending_update_refreshes_symbols_and_watches_without_download_confirmation() {
        struct TestPlatform(Rc<MinimalSoftwareWindow>);
        impl Platform for TestPlatform {
            fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
                Ok(self.0.clone())
            }
        }

        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(TestPlatform(window))).unwrap();
        let ui = AppWindow::new().unwrap();
        let (backend, _progress) = crate::firmware_loading::setup(&ui);
        let (sender, receiver) = mpsc::sync_channel(1);
        let _watcher = setup(&ui, sender.clone());
        ui.on_debug_action(|_| panic!("refreshing symbols must not download the firmware"));
        ui.set_project_dialog_open(false);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("firmware.axf");
        let original =
            include_bytes!("../../../crates/firmware-backend/tests/fixtures/arm-dwarf4.axf");
        std::fs::write(&path, original).unwrap();
        ui.set_firmware_path(path.to_str().unwrap().into());
        let mut presenter = crate::presenter::Presenter::default();
        let image = backend.load(&path).unwrap();
        presenter.apply(&ui, Event::FirmwareOpened(Box::new(image)).into());
        presenter.add_expression(&ui, "global_counter");
        assert!(presenter.saved_watches()[0].scope.is_some());
        let original_report = ui.get_firmware_report();

        let rebuilt =
            include_bytes!("../../../crates/firmware-backend/tests/fixtures/arm-dwarf5.axf");
        std::fs::write(&path, rebuilt).unwrap();
        let revision = fingerprint(&path).unwrap();
        let state = Mutex::new(Shared {
            path: path.clone(),
            sample: Some(revision),
            pending: Some(revision),
        });
        ui.set_busy(true);
        check_for_update(&ui, &state, &sender);
        assert!(receiver.try_recv().is_err());
        ui.set_busy(false);
        state.lock().unwrap().sample = None;
        check_for_update(&ui, &state, &sender);
        assert!(receiver.try_recv().is_err());
        state.lock().unwrap().sample = Some(revision);
        check_for_update(&ui, &state, &sender);
        let Ok(Command::ReloadFirmware(reload_path)) = receiver.try_recv() else {
            panic!("a stable firmware update must queue a symbol reload");
        };
        assert_eq!(reload_path, path);
        assert!(ui.get_firmware_parsing());
        let image = backend.load(&reload_path).unwrap();
        presenter.apply(&ui, Event::FirmwareReloaded(Box::new(image)).into());
        crate::firmware_loading::finish(&ui);
        assert_ne!(ui.get_firmware_report(), original_report);
        assert!(ui.get_status().starts_with("符号文件已自动刷新"));
        assert_eq!(presenter.saved_watches()[0].expression, "global_counter");
        assert!(ui.get_firmware_update_open());
        ui.invoke_respond_firmware_update(false);
        assert!(!ui.get_firmware_update_open());
        check_for_update(&ui, &state, &sender);
        assert!(receiver.try_recv().is_err());
    }
}
