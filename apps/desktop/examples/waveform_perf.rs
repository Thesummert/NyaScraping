//! CPU path preparation benchmark (excludes sampling, SVG parsing and GPU rendering).
//! Run with `cargo run -p nya-scraping --example waveform_perf --locked`.
#![allow(dead_code)]
#[path = "../src/waveform_view.rs"]
mod waveform_view;
use std::{hint::black_box, time::Instant};
use waveform_view::{PlotRenderer, COLORS, PLOT_WIDTH};

fn main() {
    for channels in [1, 6, 16] {
        let bins: Vec<Vec<_>> = (0..channels)
            .map(|channel| {
                (0..PLOT_WIDTH)
                    .map(|x| {
                        if x % 37 == 0 {
                            return None;
                        }
                        let y = (x as f64 * 0.03 + channel as f64).sin();
                        Some((y - 0.05, y + if x % 91 == 0 { 10.0 } else { 0.05 }))
                    })
                    .collect()
            })
            .collect();
        let traces: Vec<_> = bins
            .iter()
            .enumerate()
            .map(|(i, bins)| (bins.as_slice(), COLORS[i % COLORS.len()]))
            .collect();
        let mut renderer = PlotRenderer::default();
        renderer.render_if_changed(&traces, 1.0, 5.0); // Warm the envelope cache.
        const FRAMES: usize = 500;
        let start = Instant::now();
        for i in 0..FRAMES {
            // Alternate scale so every iteration generates new paths.
            black_box(renderer.render_if_changed(
                black_box(&traces),
                1.0 + (i % 2) as f64 * 0.001,
                5.0,
            ));
        }
        let changed = start.elapsed().as_secs_f64() * 1e6 / FRAMES as f64;
        renderer.render_if_changed(&traces, 1.0, 5.0);
        let start = Instant::now();
        for _ in 0..FRAMES {
            assert!(black_box(renderer.render_if_changed(black_box(&traces), 1.0, 5.0)).is_none());
        }
        let cached = start.elapsed().as_secs_f64() * 1e6 / FRAMES as f64;
        println!(
            "{channels} channels: changed {changed:.1} us/frame, unchanged {cached:.1} us/check"
        );
    }
}
