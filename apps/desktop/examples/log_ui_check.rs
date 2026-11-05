//! Offscreen check that both log tabs actually render and update their content.
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, PlatformError, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, Rgb8Pixel};
use std::rc::Rc;

slint::include_modules!();

struct TestPlatform(Rc<MinimalSoftwareWindow>);
impl Platform for TestPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.0.clone())
    }
}

fn render(window: &MinimalSoftwareWindow, pixels: &mut [Rgb8Pixel]) {
    window.request_redraw();
    assert!(window.draw_if_needed(|renderer| {
        renderer.render(pixels, 800);
    }));
}

fn content_changed(before: &[Rgb8Pixel], after: &[Rgb8Pixel]) -> bool {
    (115..200).any(|y| (20..780).any(|x| before[y * 800 + x] != after[y * 800 + x]))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(window.clone())))?;
    let ui = SystemLogWindow::new()?;
    ui.show()?;
    window.set_size(slint::PhysicalSize::new(800, 500));
    let mut pixels = vec![Rgb8Pixel::default(); 800 * 500];
    render(&window, &mut pixels);
    let blank_events = pixels.clone();
    ui.set_events("GDB connection failed: connection refused".into());
    render(&window, &mut pixels);
    assert!(
        content_changed(&blank_events, &pixels),
        "application events must appear in the log window"
    );
    let first_events = pixels.clone();
    ui.set_events("GDB connection failed: no such file".into());
    render(&window, &mut pixels);
    assert!(
        content_changed(&first_events, &pixels),
        "application events must update while open"
    );

    let position = slint::LogicalPosition::new(440., 90.);
    window.dispatch_event(WindowEvent::PointerMoved { position });
    window.dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
    window.dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Left,
    });
    render(&window, &mut pixels);
    let blank_debug = pixels.clone();
    ui.set_debug_output("GDB: remote connection closed".into());
    render(&window, &mut pixels);
    assert!(
        content_changed(&blank_debug, &pixels),
        "GDB output must appear in its tab"
    );
    println!("Log window rendering and live updates passed");
    Ok(())
}
