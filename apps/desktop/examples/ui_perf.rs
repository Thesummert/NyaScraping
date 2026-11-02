//! Headless benchmark for HiDPI redraws and large firmware-tree selection.
//! Run with `cargo run -p nya-scraping --example ui_perf --locked`.
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, PlatformError, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, Model, Rgb8Pixel};
use std::{rc::Rc, time::Instant};

#[path = "../src/firmware_browser.rs"]
mod firmware_browser;
#[path = "../src/presenter.rs"]
mod presenter;
#[path = "../src/watch_list.rs"]
mod watch_list;
slint::include_modules!();

struct BenchPlatform(Rc<MinimalSoftwareWindow>);
impl Platform for BenchPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.0.clone())
    }
}

fn redraw(window: &MinimalSoftwareWindow, pixels: &mut [Rgb8Pixel], width: usize) {
    window.request_redraw();
    assert!(window.draw_if_needed(|renderer| {
        renderer.render(pixels, width);
    }));
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(BenchPlatform(window.clone())))?;
    let ui = AppWindow::new()?;
    ui.show()?;
    window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: 2.0 });
    window.set_size(slint::PhysicalSize::new(2960, 1800));
    let mut pixels = vec![Rgb8Pixel::default(); 2960 * 1800];
    redraw(&window, &mut pixels, 2960);
    let start = Instant::now();
    for frame in 0..20 {
        window.dispatch_event(WindowEvent::PointerMoved {
            position: slint::LogicalPosition::new(30., 205. + (frame % 10) as f32 * 30.),
        });
        slint::platform::update_timers_and_animations();
        redraw(&window, &mut pixels, 2960);
    }
    println!(
        "HiDPI hover: {:.2} ms/frame",
        start.elapsed().as_secs_f64() * 1000. / 20.
    );
    let start = Instant::now();
    for frame in 0..10 {
        let width = 2960 + frame * 2;
        window.set_size(slint::PhysicalSize::new(width, 1800));
        pixels.resize(width as usize * 1800, Rgb8Pixel::default());
        redraw(&window, &mut pixels, width as usize);
    }
    println!(
        "HiDPI resize: {:.2} ms/frame",
        start.elapsed().as_secs_f64() * 100.
    );
    window.set_size(slint::PhysicalSize::new(2960, 1800));
    pixels.resize(2960 * 1800, Rgb8Pixel::default());
    let source: String = (0..2000)
        .map(|line| format!("    counter += {line}; // source line\n"))
        .collect();
    let start = Instant::now();
    ui.set_source_text(source.into());
    redraw(&window, &mut pixels, 2960);
    println!(
        "Open 2000 source lines: {:.2} ms",
        start.elapsed().as_secs_f64() * 1000.
    );
    let start = Instant::now();
    for _ in 0..10 {
        window.dispatch_event(WindowEvent::PointerScrolled {
            position: slint::LogicalPosition::new(700., 400.),
            delta_x: 0.,
            delta_y: -60.,
        });
        redraw(&window, &mut pixels, 2960);
    }
    println!(
        "Scroll source: {:.2} ms/frame",
        start.elapsed().as_secs_f64() * 100.
    );
    let mut presenter = presenter::Presenter::default();
    use nya_domain::firmware::*;
    let info = FirmwareDebugInfo {
        variables: (0..20000)
            .map(|id| DebugVariable {
                name: format!("variable_{id:05}"),
                type_name: Some("int".into()),
                byte_size: Some(4),
                declaration: None,
                function: None,
                kind: VariableKind::Global,
                location: VariableLocation::Address(id * 4),
                constant: None,
            })
            .collect(),
        ..Default::default()
    };
    let image = FirmwareImage {
        path: "benchmark.elf".into(),
        file_size: 0,
        format: "ELF".into(),
        architecture: "ARM".into(),
        kind: "Executable".into(),
        address_bits: 32,
        little_endian: true,
        entry: 0,
        has_debug_info: true,
        debug: info,
        segments: vec![],
        sections: vec![],
        symbols: vec![],
    };
    presenter.apply(
        &ui,
        nya_application::Event::FirmwareOpened(Box::new(image)).into(),
    );
    presenter.select_firmware(&ui, 0);
    assert_eq!(ui.get_firmware_tree().row_count(), 20001);
    let original_tree = ui.get_firmware_tree();
    let start = Instant::now();
    for row in 1..51 {
        presenter.select_firmware(&ui, row);
    }
    assert!(std::ptr::eq(
        original_tree.as_any(),
        ui.get_firmware_tree().as_any()
    ));
    println!(
        "Select in 20000-node tree: {:.3} ms/click",
        start.elapsed().as_secs_f64() * 20.
    );
    redraw(&window, &mut pixels, 2960);
    let start = Instant::now();
    for row in 1..21 {
        presenter.select_firmware(&ui, row);
        redraw(&window, &mut pixels, 2960);
    }
    println!(
        "Select + redraw with source and tree: {:.2} ms/frame",
        start.elapsed().as_secs_f64() * 50.
    );
    // Exercise the remaining presenter paths without a GUI event loop.
    presenter.add_selected_variable(&ui);
    presenter.add_expression(&ui, "counter");
    presenter.remove_watch(&ui, 0);
    Ok(())
}
