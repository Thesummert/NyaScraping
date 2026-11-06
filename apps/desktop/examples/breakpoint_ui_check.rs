//! Offscreen regression check for breakpoint navigation and execution markers.
#![allow(dead_code)]
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, PlatformError, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, Rgb8Pixel};
use std::rc::Rc;

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

fn render(window: &MinimalSoftwareWindow, pixels: &mut [Rgb8Pixel]) {
    // Resolve layout before the deferred navigation, then paint the new viewport.
    for _ in 0..3 {
        window.request_redraw();
        window.draw_if_needed(|renderer| {
            renderer.render(pixels, 1500);
        });
        slint::platform::update_timers_and_animations();
    }
}

fn stopped(path: &str, line: u32) -> nya_domain::DebugSnapshot {
    nya_domain::DebugSnapshot {
        state: nya_domain::SessionState::Halted,
        frames: vec![nya_domain::StackFrame {
            level: 0,
            function: "main".into(),
            file: path.into(),
            line,
            address: "0x8000010".into(),
        }],
        source: Some(nya_domain::Document {
            path: path.into(),
            text: (1..=300).map(|n| format!("int value_{n};\n")).collect(),
            encoding: "UTF-8".into(),
            bom: false,
            line_ending: nya_domain::LineEnding::Lf,
            mixed_endings: false,
        }),
        ..Default::default()
    }
}

fn marker_y(ui: &AppWindow, pixels: &[Rgb8Pixel]) -> Option<usize> {
    let x = ui.get_source_editor_x() as usize + 12;
    let top = ui.get_source_editor_y() as usize;
    let bottom = (top + ui.get_source_editor_height() as usize).min(900);
    (top..bottom).find(|&y| {
        (x.saturating_sub(8)..x + 12).any(|column| {
            let pixel = pixels[y * 1500 + column];
            pixel.r == 184 && pixel.g == 169 && pixel.b == 255
        })
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(BenchPlatform(window.clone())))?;
    let ui = AppWindow::new()?;
    ui.set_project_dialog_open(false);
    ui.show()?;
    window.set_size(slint::PhysicalSize::new(1500, 900));
    let mut pixels = vec![Rgb8Pixel::default(); 1500 * 900];
    let mut presenter = presenter::Presenter::default();
    render(&window, &mut pixels);
    ui.set_current_tab(2);
    let mut initial_stop = stopped("/fixture/main.c", 200);
    initial_stop.context_pending = true;
    presenter.apply(&ui, presenter::PreparedEvent::Debug(initial_stop, false));
    render(&window, &mut pixels);
    assert!(
        ui.get_debug_halted(),
        "the UI must show a confirmed halt before details arrive"
    );
    assert!(!ui.get_debug_running());
    assert_eq!(ui.get_current_tab(), 0);
    assert_eq!(ui.get_source_execution_line(), 200);
    assert!(
        marker_y(&ui, &pixels).is_some(),
        "plain text must scroll to and mark line 200"
    );
    presenter.apply(
        &ui,
        presenter::PreparedEvent::Debug(stopped("/fixture/main.c", 200), false),
    );
    assert!(ui.get_debug_halted());

    let clicked = Rc::new(std::cell::Cell::new(0));
    let captured = clicked.clone();
    ui.on_toggle_source_breakpoint(move |line| captured.set(line));
    ui.set_debug_running(true);
    let position = slint::LogicalPosition::new(
        ui.get_source_editor_x() - 20.,
        marker_y(&ui, &pixels).unwrap() as f32 + 5.,
    );
    window.dispatch_event(WindowEvent::PointerMoved { position });
    window.dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
    window.dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Left,
    });
    assert_eq!(
        clicked.get(),
        200,
        "gutter clicks must match the visible source line while running"
    );
    ui.set_debug_running(false);

    let rows: Vec<_> = (1..=301)
        .map(|n| HighlightLine {
            spans: slint::ModelRc::new(slint::VecModel::from(vec![HighlightSpan {
                text: format!("int value_{n};").into(),
                color: slint::Color::from_rgb_u8(220, 225, 232),
            }])),
        })
        .collect();
    ui.set_source_highlights(slint::ModelRc::new(slint::VecModel::from(rows)));
    render(&window, &mut pixels);
    assert!(
        marker_y(&ui, &pixels).is_some(),
        "async syntax highlighting must retain the execution position"
    );

    presenter.apply(
        &ui,
        presenter::PreparedEvent::Source(stopped("/fixture/other.c", 1).source.unwrap()),
    );
    assert_eq!(
        ui.get_source_execution_line(),
        0,
        "other documents must not inherit the execution marker"
    );
    ui.set_current_tab(1);
    let mut sample = stopped("/fixture/main.c", 200);
    sample.source = None;
    sample.watch_sample = true;
    presenter.apply(&ui, presenter::PreparedEvent::Debug(sample, false));
    assert_eq!(
        ui.get_current_tab(),
        1,
        "watch samples must not steal navigation"
    );
    assert!(ui.get_source_title().ends_with("other.c"));

    presenter.apply(
        &ui,
        presenter::PreparedEvent::Debug(
            nya_domain::DebugSnapshot {
                state: nya_domain::SessionState::Running,
                ..Default::default()
            },
            false,
        ),
    );
    assert_eq!(ui.get_source_execution_line(), 0);
    presenter.apply(
        &ui,
        presenter::PreparedEvent::Debug(stopped("/fixture/main.c", 250), false),
    );
    render(&window, &mut pixels);
    assert_eq!(ui.get_source_execution_line(), 250);
    assert_eq!(ui.get_current_tab(), 0);
    assert!(marker_y(&ui, &pixels).is_some());
    println!(
        "Breakpoint navigation and execution markers passed (plain text and syntax highlighting)"
    );
    Ok(())
}
