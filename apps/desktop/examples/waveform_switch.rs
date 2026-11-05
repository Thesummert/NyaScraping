//! Headless regression check for the independent-window switch.
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, PlatformError, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, Rgb8Pixel};
use std::rc::Rc;

slint::slint! {
    import { WaveformsPage } from "ui/pages/waveforms.slint";
    export component SwitchTest inherits Window {
        in-out property <bool> detached;
        in-out property <int> toggle-count;
        WaveformsPage {
            detached <=> root.detached;
            toggle-detached(value) => { root.toggle-count += 1; }
        }
    }
}

struct TestPlatform(Rc<MinimalSoftwareWindow>);
impl Platform for TestPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.0.clone())
    }
}

fn click(window: &MinimalSoftwareWindow, position: slint::LogicalPosition) {
    window.dispatch_event(WindowEvent::PointerMoved { position });
    window.dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
    window.dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Left,
    });
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(window.clone())))?;
    let ui = SwitchTest::new()?;
    ui.show()?;
    window.set_size(slint::PhysicalSize::new(900, 700));
    let mut pixels = vec![Rgb8Pixel::default(); 900 * 700];
    window.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, 900);
    });

    // Locate the switch in the header, independent of font and widget style.
    let mut switch_position = None;
    for x in (300..900).step_by(4) {
        let position = slint::LogicalPosition::new(x as f32, 12.0);
        click(&window, position);
        if ui.get_toggle_count() != 0 {
            switch_position = Some(position);
            break;
        }
    }
    let position = switch_position.expect("waveform switch was not found");
    assert!(
        ui.get_detached(),
        "clicking the switch must open the window"
    );

    // Closing the native window changes the model without clicking the switch.
    ui.set_detached(false);
    click(&window, position);
    assert!(
        ui.get_detached(),
        "the switch must reopen after external close"
    );
    assert_eq!(ui.get_toggle_count(), 2);
    click(&window, position);
    assert!(!ui.get_detached(), "the switch must also close the window");
    println!("Waveform independent-window switch synchronization: passed");
    Ok(())
}
