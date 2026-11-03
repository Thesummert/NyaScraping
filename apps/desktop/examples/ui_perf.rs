//! Headless benchmark for HiDPI redraws and large firmware-tree selection.
//! Run with `cargo run -p nya-scraping --example ui_perf --locked`.
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, PlatformError, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, Model, Rgb8Pixel};
use std::{rc::Rc, time::Instant};

#[path = "../src/acquisition.rs"]
mod acquisition;
#[path = "../src/firmware_browser.rs"]
mod firmware_browser;
#[path = "../src/presenter.rs"]
mod presenter;
#[path = "../src/watch_list.rs"]
mod watch_list;
#[path = "../src/waveform_view.rs"]
mod waveform_view;
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
                scalar_type: Some(nya_domain::waveform::ScalarType::Signed),
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
        nya_application::Event::FirmwareOpened(Box::new(image.clone())).into(),
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
    // The context-menu row must win over the previously selected variable.
    presenter.select_firmware(&ui, 1);
    presenter.plot_variable(&ui, 2);
    assert_eq!(ui.get_waveform_channels().row_count(), 1);
    assert_eq!(
        ui.get_waveform_channels().row_data(0).unwrap().name,
        "variable_00001"
    );
    assert!(ui.get_watches().row_data(1).unwrap().waveform);
    presenter.plot_variable(&ui, 2);
    assert_eq!(ui.get_waveform_channels().row_count(), 1);
    presenter.toggle_waveform(&ui, 1);
    assert_eq!(ui.get_waveform_channels().row_count(), 0);
    presenter.toggle_waveform(&ui, 1);
    ui.set_acquisition_endpoint("invalid".into());
    presenter.start_acquisition(&ui);
    assert!(!ui.get_acquisition_active());
    let acquisition = presenter.acquisition.clone();
    let sampler = acquisition.clone();
    let worker = std::thread::spawn(move || sampler.run());
    // No hardware request: exercise startup and shutdown against a local RPC fixture.
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    ui.set_acquisition_endpoint(listener.local_addr()?.to_string().into());
    let fake_openocd = std::thread::spawn(move || {
        use std::io::{BufRead, BufReader, Write};
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut stream = BufReader::new(stream);
        loop {
            let mut request = Vec::new();
            match stream.read_until(0x1a, &mut request) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    assert_eq!(
                        String::from_utf8(request).unwrap(),
                        "join [list [join [read_memory 0x4 32 1] ,]] |\x1a"
                    );
                    if stream.get_mut().write_all(b"42\x1a").is_err() {
                        break;
                    }
                }
            }
        }
    });
    presenter.start_acquisition(&ui);
    let deadline = Instant::now() + std::time::Duration::from_secs(5);
    while acquisition.state.lock().unwrap().sweeps < 10 {
        assert!(
            Instant::now() < deadline,
            "sampler did not receive RPC data"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    presenter.refresh_acquisition(&ui);
    assert_eq!(ui.get_waveform_channels().row_data(0).unwrap().value, "42");
    assert_eq!(ui.get_watches().row_data(1).unwrap().value, "42");
    redraw(&window, &mut pixels, 2960);
    if let Some(path) = std::env::var_os("NYA_UI_PREVIEW").or_else(|| std::env::args_os().nth(1)) {
        use std::io::Write;
        let mut file = std::fs::File::create(path)?;
        file.write_all(b"P6\n2960 1800\n255\n")?;
        let rgb: Vec<u8> = pixels
            .iter()
            .flat_map(|pixel| [pixel.r, pixel.g, pixel.b])
            .collect();
        file.write_all(&rgb)?;
    }
    presenter.stop_acquisition(&ui);
    assert!(!ui.get_acquisition_active());
    assert_eq!(ui.get_waveform_channels().row_count(), 1);
    assert!(ui
        .get_watches()
        .row_data(1)
        .unwrap()
        .value
        .contains("已停止"));
    presenter.remove_watch(&ui, 1);
    assert_eq!(ui.get_waveform_channels().row_count(), 0);
    acquisition.shutdown();
    worker.join().unwrap();
    fake_openocd.join().unwrap();
    presenter.apply(
        &ui,
        nya_application::Event::FirmwareOpened(Box::new(image)).into(),
    );
    assert_eq!(ui.get_waveform_channels().row_count(), 0);
    assert_eq!(ui.get_watches().row_count(), 1);
    assert_eq!(ui.get_watches().row_data(0).unwrap().expression, "counter");
    assert!(!ui.get_acquisition_active());
    println!("Waveform selection, toggles, live RPC, redraw and shutdown: passed");
    Ok(())
}
