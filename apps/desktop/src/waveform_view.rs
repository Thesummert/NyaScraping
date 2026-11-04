use slint::{Image, Rgb8Pixel, SharedPixelBuffer};

pub const PLOT_WIDTH: usize = 800;
const HEIGHT: usize = 400;
pub const COLORS: [[u8; 3]; 6] = [
    [104, 216, 202],
    [255, 191, 105],
    [166, 153, 255],
    [255, 134, 164],
    [126, 198, 255],
    [190, 225, 126],
];

pub fn render(traces: &[(&[Option<(f64, f64)>], [u8; 3])]) -> Image {
    let mut buffer = SharedPixelBuffer::<Rgb8Pixel>::new(PLOT_WIDTH as u32, HEIGHT as u32);
    let pixels = buffer.make_mut_slice();
    for (i, pixel) in pixels.iter_mut().enumerate() {
        let (x, y) = (i % PLOT_WIDTH, i / PLOT_WIDTH);
        *pixel = if y.is_multiple_of(100) || x.is_multiple_of(100) {
            Rgb8Pixel {
                r: 43,
                g: 52,
                b: 64,
            }
        } else {
            Rgb8Pixel {
                r: 17,
                g: 22,
                b: 30,
            }
        };
    }
    for &(bins, color) in traces {
        let range = bins.iter().flatten().fold(None, |range, &(low, high)| {
            Some(match range {
                Some((min, max)) => (f64::min(min, low), f64::max(max, high)),
                None => (low, high),
            })
        });
        let Some((min, max)) = range else {
            continue;
        };
        // Normalize before subtracting to avoid overflow for opposite-sign large f64s.
        let scale = min.abs().max(max.abs()).max(1.0);
        let low = min / scale;
        let span = max / scale - low;
        let y_of = |value: f64| {
            let unit = if span > 0.0 {
                (value / scale - low) / span
            } else {
                0.5
            };
            (HEIGHT as f64 * (0.9 - unit * 0.8)).clamp(0.0, (HEIGHT - 1) as f64) as i32
        };
        let pixel = Rgb8Pixel {
            r: color[0],
            g: color[1],
            b: color[2],
        };
        let mut previous: Option<(usize, i32)> = None;
        for (x, bin) in bins.iter().take(PLOT_WIDTH).enumerate() {
            if let Some((low, high)) = bin {
                let y1 = y_of(*high);
                let y2 = y_of(*low);
                for y in y1.min(y2)..=y1.max(y2) {
                    pixels[y as usize * PLOT_WIDTH + x] = pixel;
                }
                let middle = (y1 + y2) / 2;
                if let Some((px, py)) = previous {
                    // Interpolate ordinary low-rate samples, leaving gaps over 0.5 s.
                    if x - px <= PLOT_WIDTH / 10 {
                        let mut last_y = py;
                        for column in px + 1..=x {
                            let y = py + (middle - py) * (column - px) as i32 / (x - px) as i32;
                            for row in last_y.min(y)..=last_y.max(y) {
                                pixels[row as usize * PLOT_WIDTH + column] = pixel;
                            }
                            last_y = y;
                        }
                    }
                }
                previous = Some((x, middle));
            }
        }
    }
    Image::from_rgb8(buffer)
}

pub fn format_value(value: f64) -> String {
    if value == 0.0 || (value.abs() >= 0.001 && value.abs() < 1e7) {
        format!("{value:.6}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    } else {
        format!("{value:.5e}")
    }
}
