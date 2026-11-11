//! Offscreen regression check for panel dividers at the center pane's minimum width.
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

fn render(window: &MinimalSoftwareWindow, pixels: &mut [Rgb8Pixel], width: usize) {
    window.request_redraw();
    assert!(window.draw_if_needed(|renderer| {
        renderer.render(pixels, width);
    }));
}

fn divider_x(pixels: &[Rgb8Pixel], width: usize, y: usize, scale: f32) -> f32 {
    let background = Rgb8Pixel::new(16, 18, 27);
    let margin = (4. * scale).round() as usize;
    let half_height = (10. * scale).round() as usize;
    let columns: Vec<_> = ((400. * scale) as usize..width - margin)
        .filter(|&x| {
            let color = pixels[y * width + x];
            color != background
                && pixels[y * width + x - margin] == background
                && pixels[y * width + x + margin] == background
                && (y - half_height..=y + half_height).all(|row| pixels[row * width + x] == color)
        })
        .collect();
    assert!(!columns.is_empty(), "the right divider must remain visible");
    (columns[0] + columns[columns.len() - 1] + 1) as f32 / (2. * scale)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(window.clone())))?;
    let ui = AppWindow::new()?;
    ui.set_project_dialog_open(false);
    ui.show()?;
    for scale in [1., 1.25, 2.] {
        window.dispatch_event(WindowEvent::ScaleFactorChanged {
            scale_factor: scale,
        });
        for (logical_width, travel) in [(1500., 476.), (1180., 150.)] {
            let width = (logical_width * scale) as usize;
            let height = (900. * scale) as usize;
            window.set_size(slint::PhysicalSize::new(width as u32, height as u32));
            let mut pixels = vec![Rgb8Pixel::default(); width * height];
            for tab in 0..4 {
                ui.set_current_tab(tab);
                render(&window, &mut pixels, width);
                let y = 494.; // Workspace center, below the menu and toolbar.
                let row = (y * scale).round() as usize;
                // Reverse while pressed, release at the resized edge, then grab
                // that edge again to verify the hit area moves with the grip.
                for steps in [&[-0.25, -0.5, -1., -0.75, -0.5][..], &[0.25, 0.5][..]] {
                    let start = divider_x(&pixels, width, row, scale);
                    let position = slint::LogicalPosition::new(start, y);
                    window.dispatch_event(WindowEvent::PointerMoved { position });
                    window.dispatch_event(WindowEvent::PointerPressed {
                        position,
                        button: PointerEventButton::Left,
                    });
                    for step in steps {
                        let x = start + step * travel;
                        window.dispatch_event(WindowEvent::PointerMoved {
                            position: slint::LogicalPosition::new(x, y),
                        });
                        render(&window, &mut pixels, width);
                        let actual = divider_x(&pixels, width, row, scale);
                        assert!(
                            (actual - x).abs() <= 1.,
                            "scale {scale}, width {logical_width}, tab {tab}: divider must follow the pointer at {x}, rendered at {actual}"
                        );
                    }
                    window.dispatch_event(WindowEvent::PointerReleased {
                        position: slint::LogicalPosition::new(
                            start + steps.last().unwrap() * travel,
                            y,
                        ),
                        button: PointerEventButton::Left,
                    });
                }
            }
            println!("Panel resizing passed for all tabs: width {logical_width}, scale {scale}");
        }
    }
    Ok(())
}
