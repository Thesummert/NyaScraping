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

pub struct Scale {
    scale: f64,
    center: f64,
    span: f64,
}

impl Scale {
    pub fn from_bins(bins: &[Option<(f64, f64)>], zoom: f64) -> Option<Self> {
        let (min, max) = bins.iter().flatten().fold(None, |range, &(low, high)| {
            Some(match range {
                Some((min, max)) => (f64::min(min, low), f64::max(max, high)),
                None => (low, high),
            })
        })?;
        let scale = min.abs().max(max.abs()).max(1.0);
        let low = min / scale;
        let high = max / scale;
        let center = low / 2.0 + high / 2.0;
        let span = if high > low {
            (high - low) / 0.8
        } else {
            (center.abs() * 0.2).max(1.0 / scale)
        } / zoom;
        Some(Self {
            scale,
            center,
            span,
        })
    }

    pub fn tick(&self, index: usize) -> String {
        let normalized = self.center + (0.5 - index as f64 / 4.0) * self.span;
        format_value((normalized * self.scale).clamp(-f64::MAX, f64::MAX))
    }

    fn y(&self, value: f64) -> f64 {
        (0.5 - (value / self.scale - self.center) / self.span) * (HEIGHT - 1) as f64
    }
}

pub fn render(
    traces: &[(&[Option<(f64, f64)>], [u8; 3])],
    zoom: f64,
    window_seconds: f64,
) -> Image {
    let mut buffer = SharedPixelBuffer::<Rgb8Pixel>::new(PLOT_WIDTH as u32, HEIGHT as u32);
    let pixels = buffer.make_mut_slice();
    for (i, pixel) in pixels.iter_mut().enumerate() {
        let (x, y) = (i % PLOT_WIDTH, i / PLOT_WIDTH);
        *pixel = if y.is_multiple_of(100) || x.is_multiple_of(200) {
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
        let Some(scale) = Scale::from_bins(bins, zoom) else {
            continue;
        };
        let y_of = |value: f64| scale.y(value);
        let pixel = Rgb8Pixel {
            r: color[0],
            g: color[1],
            b: color[2],
        };
        let mut previous: Option<(usize, f64)> = None;
        for (x, bin) in bins.iter().take(PLOT_WIDTH).enumerate() {
            if let Some((low, high)) = bin {
                let y1 = y_of(*high);
                let y2 = y_of(*low);
                let draw = |pixels: &mut [Rgb8Pixel], column: usize, a: f64, b: f64| {
                    let low = a.min(b).max(0.0);
                    let high = a.max(b).min((HEIGHT - 1) as f64);
                    if low <= high {
                        for row in low as usize..=high as usize {
                            pixels[row * PLOT_WIDTH + column] = pixel;
                        }
                    }
                };
                draw(pixels, x, y1, y2);
                let middle = (y1 + y2) / 2.0;
                if let Some((px, py)) = previous {
                    if x - px <= ((PLOT_WIDTH as f64 * 0.5 / window_seconds).ceil() as usize).max(1)
                    {
                        let mut last_y = py;
                        for column in px + 1..=x {
                            let y = py + (middle - py) * (column - px) as f64 / (x - px) as f64;
                            draw(pixels, column, last_y, y);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_match_drawn_scale_and_zoom() {
        let bins = [Some((0.0, 100.0))];
        let scale = Scale::from_bins(&bins, 1.0).unwrap();
        assert_eq!(scale.tick(0), "112.5");
        assert_eq!(scale.tick(2), "50");
        assert_eq!(scale.tick(4), "-12.5");
        assert!((scale.y(50.0) - 199.5).abs() < 0.001);
        let zoomed = Scale::from_bins(&bins, 2.0).unwrap();
        assert_eq!(zoomed.tick(0), "81.25");
        assert_eq!(zoomed.tick(4), "18.75");
        assert!(zoomed.y(100.0) < 0.0);
    }

    #[test]
    fn constant_and_extreme_values_have_usable_scales() {
        let constant = Scale::from_bins(&[Some((0.0, 0.0))], 1.0).unwrap();
        assert_eq!(constant.tick(0), "0.5");
        assert_eq!(constant.tick(4), "-0.5");
        let extreme = Scale::from_bins(&[Some((-f64::MAX, f64::MAX))], 100.0).unwrap();
        assert!(extreme.y(0.0).is_finite());
        assert!(!extreme.tick(0).contains("inf"));
        assert!(Scale::from_bins(&[None], 1.0).is_none());
    }
}
