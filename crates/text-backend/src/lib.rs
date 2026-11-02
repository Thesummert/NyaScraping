//! Native Rust file I/O, encoding conversion, and newline handling.
#![forbid(unsafe_code)]

mod encoding;
mod newline;

use nya_domain::{Document, Error, Result, TextBackend};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
};

/// Bound encoded input and output, including files that grow while being read.
pub const MAX_FILE_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default)]
pub struct NativeTextBackend;

fn io_error(error: impl std::fmt::Display) -> Error {
    Error(error.to_string())
}

impl NativeTextBackend {
    /// Override heuristic detection for a legacy file or BOM-less UTF-16.
    /// A conflicting BOM or malformed input returns an error without replacement.
    pub fn load_with_encoding(&self, path: &Path, encoding: Option<&str>) -> Result<Document> {
        let file = fs::File::open(path).map_err(io_error)?;
        let metadata = file.metadata().map_err(io_error)?;
        if !metadata.is_file() {
            return Err(Error("Source must be a regular file".into()));
        }
        if metadata.len() > MAX_FILE_BYTES {
            return Err(Error("File exceeds 128 MiB".into()));
        }
        let mut bytes = Vec::new();
        file.take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(io_error)?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(Error("File exceeds 128 MiB".into()));
        }
        let decoded = encoding::decode(&bytes, encoding)?;
        let (text, line_ending, mixed_endings) = newline::normalize(&decoded.text);
        Ok(Document {
            path: path.to_owned(),
            text,
            encoding: decoded.encoding.name().into(),
            bom: decoded.bom,
            line_ending,
            mixed_endings,
        })
    }
}

impl TextBackend for NativeTextBackend {
    fn load(&self, path: &Path) -> Result<Document> {
        self.load_with_encoding(path, None)
    }

    fn save(&self, document: &Document) -> Result<()> {
        let text = newline::render(&document.text, document.line_ending);
        let bytes = encoding::encode(&text, &document.encoding, document.bom)?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(Error("Encoded file exceeds 128 MiB".into()));
        }
        // Follow existing symlinks so saving does not replace the link itself.
        let target = match fs::symlink_metadata(&document.path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                fs::canonicalize(&document.path).map_err(io_error)?
            }
            Ok(_) => document.path.clone(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => document.path.clone(),
            Err(error) => return Err(io_error(error)),
        };
        let existing = match fs::metadata(&target) {
            Ok(metadata) => {
                if !metadata.is_file() || metadata.permissions().readonly() {
                    return Err(Error("Destination is not a writable regular file".into()));
                }
                Some(metadata)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(io_error(error)),
        };
        let parent = target
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut temporary = tempfile::Builder::new()
            .prefix(".nya-save-")
            .tempfile_in(parent)
            .map_err(io_error)?;
        temporary.write_all(&bytes).map_err(io_error)?;
        if let Some(metadata) = existing {
            temporary
                .as_file()
                .set_permissions(metadata.permissions())
                .map_err(io_error)?;
        }
        temporary.as_file().sync_all().map_err(io_error)?;
        temporary.persist(&target).map_err(io_error)?;
        Ok(())
    }
}
