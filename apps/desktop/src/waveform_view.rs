pub const PLOT_WIDTH: usize = 800;
const HEIGHT: usize = 400;
// Subpixel coordinates without floating-point formatting in the frame loop.
// Keep the Path viewbox in waveforms.slint in sync with these dimensions.
const PATH_SCALE: f64 = 16.0;

#[derive(Debug, PartialEq)]
pub struct PlotPath {
    pub commands: String,
    pub color: [u8; 3],
}
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

type Trace = (Vec<Option<(f64, f64)>>, [u8; 3]);

#[derive(Default)]
pub struct PlotRenderer {
    previous: Option<(Vec<Trace>, f64, f64)>,
}

impl PlotRenderer {
    /// Keep the existing native paths when the envelope inputs match.
    pub fn render_if_changed(
        &mut self,
        traces: &[(&[Option<(f64, f64)>], [u8; 3])],
        zoom: f64,
        window_seconds: f64,
    ) -> Option<Vec<PlotPath>> {
        if self
            .previous
            .as_ref()
            .is_some_and(|(previous, z, seconds)| {
                *z == zoom
                    && *seconds == window_seconds
                    && previous.len() == traces.len()
                    && previous
                        .iter()
                        .zip(traces)
                        .all(|((a, ac), (b, bc))| a.as_slice() == *b && ac == bc)
            })
        {
            return None;
        }
        let paths = render_paths(traces, zoom, window_seconds);
        let (previous, z, seconds) = self.previous.get_or_insert_with(Default::default);
        previous.resize_with(traces.len(), || (Vec::new(), [0; 3]));
        for ((cached_bins, cached_color), (bins, color)) in previous.iter_mut().zip(traces) {
            cached_bins.clear();
            cached_bins.extend_from_slice(bins);
            *cached_color = *color;
        }
        *z = zoom;
        *seconds = window_seconds;
        Some(paths)
    }
}

fn render_paths(
    traces: &[(&[Option<(f64, f64)>], [u8; 3])],
    zoom: f64,
    window_seconds: f64,
) -> Vec<PlotPath> {
    let max_gap = ((PLOT_WIDTH as f64 * 0.5 / window_seconds).ceil() as usize).max(1);
    traces
        .iter()
        .map(|&(bins, color)| {
            let mut commands = String::with_capacity(bins.len().min(PLOT_WIDTH) * 24);
            let mut envelope = String::new();
            if let Some(scale) = Scale::from_bins(bins, zoom) {
                let mut previous: Option<usize> = None;
                for (x, bin) in bins.iter().take(PLOT_WIDTH).enumerate() {
                    let Some((low, high)) = bin else { continue };
                    let top = scale.y(*high);
                    let bottom = scale.y(*low);
                    let y1 = (top * PATH_SCALE).round() as i32;
                    let y2 = (bottom * PATH_SCALE).round() as i32;
                    let middle = ((top / 2.0 + bottom / 2.0) * PATH_SCALE).round() as i32;
                    let path_x = (x as f64 * PATH_SCALE) as i32;
                    // A vertical segment preserves each bin's min/max, including narrow spikes.
                    // Join bin centers only across the same short gaps as the old rasterizer.
                    if previous.is_none_or(|px| x - px > max_gap) {
                        append_point(&mut commands, 'M', path_x, middle);
                    }
                    // The first zero-length segment also displays isolated samples
                    // with the Path's round line cap.
                    append_point(&mut commands, 'L', path_x, middle);
                    if y1 != y2 {
                        append_point(&mut envelope, 'M', path_x, y1);
                        append_point(&mut envelope, 'L', path_x, y2);
                    }
                    previous = Some(x);
                }
            }
            commands.push_str(&envelope);
            PlotPath { commands, color }
        })
        .collect()
}

fn append_point(commands: &mut String, command: char, x: i32, y: i32) {
    let mut buffer = itoa::Buffer::new();
    commands.push(command);
    commands.push_str(buffer.format(x));
    commands.push(' ');
    commands.push_str(buffer.format(y));
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
    fn paths_preserve_spikes_and_break_long_gaps() {
        let spikes = render_paths(&[(&[Some((0.0, 100.0))], COLORS[0])], 1.0, 5.0);
        assert_eq!(spikes[0].commands, "M0 3192L0 3192M0 638L0 5746");
        let bins = [Some((0.0, 0.0)), None, Some((0.0, 0.0))];
        let long = render_paths(&[(&bins, COLORS[0])], 1.0, 3600.0);
        assert_eq!(long[0].commands, "M0 3192L0 3192M32 3192L32 3192");
        let short = render_paths(&[(&bins, COLORS[0])], 1.0, 0.05);
        assert_eq!(short[0].commands, "M0 3192L0 3192L32 3192");
    }

    #[test]
    fn cached_plot_invalidates_on_data_color_scale_window_and_channel_changes() {
        let mut renderer = PlotRenderer::default();
        let mut bins = vec![Some((0.0, 1.0)); PLOT_WIDTH];
        assert!(renderer
            .render_if_changed(&[(&bins, COLORS[0])], 1.0, 5.0)
            .is_some());
        assert!(renderer
            .render_if_changed(&[(&bins, COLORS[0])], 1.0, 5.0)
            .is_none());
        bins[400] = Some((-100.0, 100.0));
        assert!(renderer
            .render_if_changed(&[(&bins, COLORS[0])], 1.0, 5.0)
            .is_some());
        assert!(renderer
            .render_if_changed(&[(&bins, COLORS[1])], 1.0, 5.0)
            .is_some());
        assert!(renderer
            .render_if_changed(&[(&bins, COLORS[1])], 2.0, 5.0)
            .is_some());
        assert!(renderer
            .render_if_changed(&[(&bins, COLORS[1])], 2.0, 10.0)
            .is_some());
        let blank = renderer.render_if_changed(&[], 2.0, 10.0).unwrap();
        assert!(blank.is_empty());
        assert!(renderer.render_if_changed(&[], 2.0, 10.0).is_none());
    }

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
