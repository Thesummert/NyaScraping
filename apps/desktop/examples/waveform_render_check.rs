//! Verify the native waveform Path in software, or smoke-test WGPU with --gpu.
#![allow(dead_code)]
#[path = "../src/waveform_view.rs"]
mod waveform_view;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, PlatformError, WindowAdapter};
use slint::{ComponentHandle, ModelRc, Rgb8Pixel, VecModel};
use std::{rc::Rc, time::Duration};

slint::slint! {
    import { WaveformPlot, WaveformPath } from "ui/pages/waveforms.slint";
    export { WaveformPath }
    export component PlotCheck inherits Window {
        width: 800px;
        height: 400px;
        in property <[WaveformPath]> paths;
        WaveformPlot { paths: root.paths; }
    }
}

struct TestPlatform(Rc<MinimalSoftwareWindow>);
impl Platform for TestPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.0.clone())
    }
}

fn paths(zoom: f64) -> ModelRc<WaveformPath> {
    let mut bins = vec![Some((0.0, 0.0)); 800];
    bins[400] = Some((0.0, 100.0));
    bins[500..600].fill(None);
    let paths = waveform_view::PlotRenderer::default()
        .render_if_changed(&[(&bins, waveform_view::COLORS[0])], zoom, 5.0)
        .unwrap();
    ModelRc::from(Rc::new(VecModel::from(
        paths
            .into_iter()
            .map(|path| WaveformPath {
                commands: path.commands.into(),
                tint: slint::Color::from_rgb_u8(path.color[0], path.color[1], path.color[2]),
            })
            .collect::<Vec<_>>(),
    )))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().any(|arg| arg == "--gpu") {
        slint::BackendSelector::new()
            .backend_name("winit".into())
            .renderer_name("skia-wgpu".into())
            .select()?;
        let ui = PlotCheck::new()?;
        ui.set_paths(paths(1.0));
        let weak = ui.as_weak();
        let frame = std::cell::Cell::new(0);
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(17),
            move || {
                let count = frame.get() + 1;
                frame.set(count);
                if let Some(ui) = weak.upgrade() {
                    ui.set_paths(paths(1.0 + (count % 10) as f64 * 0.1));
                    if count == 120 {
                        // A readback verifies rendered content, rather than only
                        // checking that the timer/event loop ran successfully.
                        let snapshot = ui.window().take_snapshot().unwrap();
                        assert!(snapshot.as_slice().iter().any(|p| p.g > 100 && p.b > 100));
                        println!(
                            "WGPU snapshot: {} x {}, waveform pixels present",
                            snapshot.width(),
                            snapshot.height()
                        );
                    }
                }
                if count == 120 {
                    slint::quit_event_loop().unwrap();
                }
            },
        );
        ui.run()?;
        println!("WGPU waveform animation: passed");
        return Ok(());
    }
    let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    slint::platform::set_platform(Box::new(TestPlatform(window.clone())))?;
    let ui = PlotCheck::new()?;
    ui.show()?;
    window.set_size(slint::PhysicalSize::new(800, 400));
    let mut pixels = vec![Rgb8Pixel::default(); 800 * 400];
    for zoom in [1.0, 100.0] {
        ui.set_paths(paths(zoom));
        assert!(window.draw_if_needed(|renderer| {
            renderer.render(&mut pixels, 800);
        }));
        let is_trace = |x: usize, y: usize| {
            let p = pixels[y * 800 + x];
            p.g > 100 && p.b > 100
        };
        assert!(
            (397..403).any(|x| (98..103).any(|y| is_trace(x, y))),
            "spike missing at zoom {zoom}"
        );
        assert!(
            !(548..553).any(|x| (355..363).any(|y| is_trace(x, y))),
            "long gap was connected"
        );
    }
    ui.set_paths(ModelRc::default());
    assert!(window.draw_if_needed(|renderer| {
        renderer.render(&mut pixels, 800);
    }));
    assert!(
        pixels.iter().all(|pixel| pixel.g < 100),
        "cleared paths left stale traces"
    );
    println!("Native waveform paths: spike, gap, zoom clipping and clearing passed");
    Ok(())
}
