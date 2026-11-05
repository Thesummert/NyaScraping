use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

pub const MIN_SIZE: i32 = 8;
pub const MAX_SIZE: i32 = 48;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SourceFont {
    pub family: String,
    pub size: i32,
}

impl Default for SourceFont {
    fn default() -> Self {
        Self {
            family: "monospace".into(),
            size: 14,
        }
    }
}

impl SourceFont {
    pub fn normalized(mut self) -> Self {
        self.family = self.family.trim().to_owned();
        if self.family.is_empty()
            || self.family.len() > 256
            || self.family.chars().any(char::is_control)
        {
            self.family = "monospace".into();
        }
        self.size = self.size.clamp(MIN_SIZE, MAX_SIZE);
        self
    }
}

pub fn contains_point(point: (f64, f64), rect: (f32, f32, f32, f32)) -> bool {
    let (x, y, width, height) = rect;
    width > 0.0
        && height > 0.0
        && point.0 >= x as f64
        && point.0 < (x + width) as f64
        && point.1 >= y as f64
        && point.1 < (y + height) as f64
}

pub fn wheel_steps(accumulator: &mut f64, amount: f64) -> i32 {
    if accumulator.signum() != amount.signum() {
        *accumulator = 0.0;
    }
    *accumulator += amount;
    let steps = (accumulator.trunc() as i32).clamp(-4, 4);
    *accumulator -= steps as f64;
    steps
}

pub fn load() -> SourceFont {
    crate::project::config_dir()
        .and_then(|dir| fs::read(dir.join("source-font.json")).ok())
        .and_then(|data| serde_json::from_slice::<SourceFont>(&data).ok())
        .unwrap_or_default()
        .normalized()
}

fn write_to(dir: &Path, font: &SourceFont) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut file = tempfile::NamedTempFile::new_in(dir).map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(&mut file, &font.clone().normalized())
        .map_err(|e| e.to_string())?;
    file.persist(dir.join("source-font.json"))
        .map_err(|e| e.error.to_string())?;
    Ok(())
}

pub fn save(font: &SourceFont) -> Result<(), String> {
    let dir = crate::project::config_dir().ok_or("无法确定设置目录")?;
    write_to(&dir, font)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_and_persists_settings() {
        let dir = tempfile::tempdir().unwrap();
        let font = SourceFont {
            family: "  Fira Code  ".into(),
            size: 100,
        };
        write_to(dir.path(), &font).unwrap();
        let saved: SourceFont =
            serde_json::from_slice(&fs::read(dir.path().join("source-font.json")).unwrap())
                .unwrap();
        assert_eq!(
            saved,
            SourceFont {
                family: "Fira Code".into(),
                size: MAX_SIZE
            }
        );
        assert_eq!(
            SourceFont {
                family: " ".into(),
                size: 0
            }
            .normalized(),
            SourceFont {
                family: "monospace".into(),
                size: MIN_SIZE
            }
        );
    }

    #[test]
    fn zoom_limits_and_viewport_hit_test() {
        assert!(contains_point((12.0, 25.0), (10.0, 20.0, 100.0, 80.0)));
        assert!(!contains_point((110.0, 25.0), (10.0, 20.0, 100.0, 80.0)));
        assert!(!contains_point((10.0, 20.0), (10.0, 20.0, 0.0, 80.0)));
        let mut amount = 0.0;
        assert_eq!(wheel_steps(&mut amount, 0.4), 0);
        assert_eq!(wheel_steps(&mut amount, 0.7), 1);
        assert_eq!(wheel_steps(&mut amount, -1.0), -1);
        assert_eq!(wheel_steps(&mut amount, 100.0), 4);
        assert_eq!((MAX_SIZE + 4).clamp(MIN_SIZE, MAX_SIZE), MAX_SIZE);
        assert_eq!((MIN_SIZE - 4).clamp(MIN_SIZE, MAX_SIZE), MIN_SIZE);
    }
}
