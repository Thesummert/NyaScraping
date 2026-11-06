//! Offscreen firmware loading check. Optional arguments: ELF path, progress screenshot path.
#![allow(dead_code)]
use nya_domain::firmware::FirmwareBackend;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, PlatformError, WindowAdapter};
use slint::{ComponentHandle, Model, Rgb8Pixel};
use std::{
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

#[path = "../src/acquisition.rs"]
mod acquisition;
#[path = "../src/firmware_browser.rs"]
mod firmware_browser;
#[path = "../src/firmware_loading.rs"]
mod firmware_loading;
#[path = "../src/presenter.rs"]
mod presenter;
#[path = "../src/watch_list.rs"]
mod watch_list;
#[path = "../src/waveform_view.rs"]
mod waveform_view;
slint::include_modules!();

struct TestPlatform(Rc<MinimalSoftwareWindow>);
impl Platform for TestPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.0.clone())
    }
}

fn render(window: &MinimalSoftwareWindow, pixels: &mut [Rgb8Pixel]) {
    slint::platform::update_timers_and_animations();
    for _ in 0..2 {
        window.request_redraw();
        window.draw_if_needed(|renderer| {
            renderer.render(pixels, 1500);
        });
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../crates/firmware-backend/tests/fixtures/arm-dwarf4.axf"
            ))
        });
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(window.clone())))?;
    let ui = AppWindow::new()?;
    ui.set_project_dialog_open(false);
    let (backend, timer) = firmware_loading::setup(&ui);
    ui.show()?;
    window.set_size(slint::PhysicalSize::new(1500, 900));
    let mut pixels = vec![Rgb8Pixel::default(); 1500 * 900];
    render(&window, &mut pixels);
    let blank = pixels.clone();

    ui.invoke_begin_firmware_parse(path.to_string_lossy().as_ref().into());
    assert!(ui.get_firmware_parsing());
    let mut presenter = presenter::Presenter::default();
    presenter.apply(
        &ui,
        presenter::PreparedEvent::Tools(Default::default(), "扫描完成".into()),
    );
    assert!(
        ui.get_busy(),
        "unrelated background events must not unlock the UI during parsing"
    );
    render(&window, &mut pixels);
    assert_ne!(
        blank, pixels,
        "the loading dialog must be visible before parsing starts"
    );
    let started = Instant::now();
    let image = backend.load(&path)?;
    let parsed = started.elapsed();
    std::thread::sleep(Duration::from_millis(45));
    render(&window, &mut pixels);
    assert!(ui.get_firmware_parse_progress() >= 0.95);
    assert!(ui.get_firmware_parse_details().contains("秒"));
    if let Some(screenshot) = std::env::args_os().nth(2) {
        let bytes: Vec<_> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
        image::save_buffer(screenshot, &bytes, 1500, 900, image::ColorType::Rgb8)?;
    }
    backend.preparing_view();
    let event =
        presenter::PreparedEvent::from(nya_application::Event::FirmwareOpened(Box::new(image)));
    let prepared = started.elapsed();
    presenter.apply(&ui, event);
    firmware_loading::finish(&ui);
    assert!(!ui.get_firmware_parsing());
    assert!(ui.get_firmware_tree().row_count() > 0);
    assert!(ui.get_status().starts_with("解析完成"));
    println!(
        "ELF: {:.3}s; parse + prepare: {:.3}s; through UI model update: {:.3}s",
        parsed.as_secs_f64(),
        prepared.as_secs_f64(),
        started.elapsed().as_secs_f64()
    );

    ui.invoke_begin_firmware_parse("missing.elf".into());
    assert_eq!(
        ui.get_firmware_parse_progress(),
        0.0,
        "a new task must reset old progress"
    );
    assert!(backend
        .load(&std::env::temp_dir().join("nya-firmware-ui-check-missing/input.elf"))
        .is_err());
    presenter.apply(&ui, presenter::PreparedEvent::Failed("无法读取固件".into()));
    firmware_loading::finish(&ui);
    assert!(!ui.get_firmware_parsing());
    assert!(!ui.get_busy());
    assert!(
        ui.get_firmware_tree().row_count() > 0,
        "failed loads must retain the previous firmware"
    );
    timer.stop();
    println!("Firmware dialog rendering, progress updates, reset, and failure recovery passed");
    Ok(())
}
