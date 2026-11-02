use nya_domain::{Document, LineEnding, TextBackend};
use nya_text_backend::{NativeTextBackend, MAX_FILE_BYTES};
use std::{fs, path::PathBuf};

fn document(path: PathBuf) -> Document {
    Document {
        path,
        text: "变量 🎛️\n第二行\n".into(),
        encoding: "UTF-8".into(),
        bom: true,
        line_ending: LineEnding::CrLf,
        mixed_endings: false,
    }
}

#[test]
fn utf8_bom_unicode_path_and_crlf_round_trip() {
    let directory = tempfile::tempdir().unwrap();
    let original = document(directory.path().join("源码.txt"));
    NativeTextBackend.save(&original).unwrap();
    let bytes = fs::read(&original.path).unwrap();
    assert_eq!(bytes, "\u{feff}变量 🎛️\r\n第二行\r\n".as_bytes());
    let loaded = NativeTextBackend.load(&original.path).unwrap();
    assert_eq!(loaded.text, original.text);
    assert_eq!(loaded.encoding, "UTF-8");
    assert_eq!(loaded.line_ending, LineEnding::CrLf);
    assert!(loaded.bom);
    assert!(!loaded.mixed_endings);
}

#[test]
fn utf16_both_endiannesses_with_and_without_bom_round_trip() {
    let directory = tempfile::tempdir().unwrap();
    for (encoding, prefix) in [("UTF-16LE", [0xff, 0xfe]), ("UTF-16BE", [0xfe, 0xff])] {
        for bom in [false, true] {
            let mut original = document(directory.path().join(format!("{encoding}-{bom}.txt")));
            original.encoding = encoding.into();
            original.bom = bom;
            NativeTextBackend.save(&original).unwrap();
            let bytes = fs::read(&original.path).unwrap();
            assert_eq!(bytes.starts_with(&prefix), bom);
            let loaded = if bom {
                NativeTextBackend.load(&original.path).unwrap()
            } else {
                NativeTextBackend
                    .load_with_encoding(&original.path, Some(encoding))
                    .unwrap()
            };
            assert_eq!(loaded.text, original.text);
            assert_eq!(loaded.encoding, encoding);
            assert_eq!(loaded.bom, bom);
            assert_eq!(loaded.line_ending, original.line_ending);
        }
    }
}

#[test]
fn bom_is_authoritative_and_invalid_unicode_is_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("invalid.txt");
    for bytes in [
        &[0xef, 0xbb, 0xbf, 0xff][..], // invalid UTF-8 after BOM
        &[0xff, 0xfe, 0x00, 0xd8][..], // unpaired UTF-16 surrogate
        &[0xfe, 0xff, 0x00][..],       // odd UTF-16 byte count
        &[0xff, 0xfe, 0x00, 0x00][..], // unsupported UTF-32
    ] {
        fs::write(&path, bytes).unwrap();
        assert!(NativeTextBackend.load(&path).is_err());
    }
    fs::write(&path, b"\xef\xbb\xbfhello").unwrap();
    assert!(NativeTextBackend
        .load_with_encoding(&path, Some("GBK"))
        .is_err());
}

#[test]
fn legacy_encodings_can_be_selected_explicitly() {
    let directory = tempfile::tempdir().unwrap();
    for (encoding, text) in [("GBK", "中文源码\n"), ("windows-1252", "café\n")] {
        let mut original = document(directory.path().join(format!("{encoding}.txt")));
        original.encoding = encoding.into();
        original.text = text.into();
        original.bom = false;
        NativeTextBackend.save(&original).unwrap();
        let loaded = NativeTextBackend
            .load_with_encoding(&original.path, Some(encoding))
            .unwrap();
        assert_eq!(loaded.text, text);
        assert!(!loaded.bom);
    }
}

#[test]
fn legacy_detection_reads_chinese_source() {
    let directory = tempfile::tempdir().unwrap();
    let mut original = document(directory.path().join("legacy.txt"));
    original.encoding = "GBK".into();
    original.bom = false;
    original.text =
        "这是一个用于测试中文字符编码检测的源码文件。\n变量、注释和函数名称包含中文内容。\n"
            .repeat(4);
    NativeTextBackend.save(&original).unwrap();
    assert_eq!(
        NativeTextBackend.load(&original.path).unwrap().text,
        original.text
    );
}

#[test]
fn mixed_newlines_are_reported_and_saved_without_doubling_cr() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("mixed.txt");
    fs::write(&path, "一\r\n二\r三\n四\u{2028}五\u{2029}").unwrap();
    let mut loaded = NativeTextBackend.load(&path).unwrap();
    assert!(loaded.mixed_endings);
    assert_eq!(loaded.line_ending, LineEnding::CrLf);
    assert_eq!(loaded.text, "一\n二\n三\n四\u{2028}五\u{2029}");
    for (ending, separator) in [
        (LineEnding::Lf, "\n"),
        (LineEnding::CrLf, "\r\n"),
        (LineEnding::Cr, "\r"),
    ] {
        loaded.line_ending = ending;
        loaded.text = "a\r\nb\rc\n".into();
        NativeTextBackend.save(&loaded).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            format!("a{separator}b{separator}c{separator}")
        );
    }
}

#[test]
fn encoding_errors_preserve_existing_file_and_leave_no_temporary_files() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("keep.txt");
    fs::write(&path, b"original").unwrap();
    let mut original = document(path.clone());
    for (encoding, bom) in [
        ("windows-1252", false),
        ("GBK", true),
        ("not-an-encoding", false),
    ] {
        original.encoding = encoding.into();
        original.bom = bom;
        assert!(NativeTextBackend.save(&original).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"original");
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}

#[test]
fn empty_missing_directory_and_oversized_files_are_handled() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("empty.txt");
    fs::write(&path, b"").unwrap();
    let empty = NativeTextBackend.load(&path).unwrap();
    assert!(empty.text.is_empty());
    assert_eq!(empty.encoding, "UTF-8");
    assert!(!empty.bom);
    assert!(NativeTextBackend
        .load(&directory.path().join("missing.txt"))
        .is_err());
    assert!(NativeTextBackend.load(directory.path()).is_err());
    let sparse = fs::File::create(directory.path().join("oversized.txt")).unwrap();
    sparse.set_len(MAX_FILE_BYTES + 1).unwrap();
    assert!(NativeTextBackend
        .load(&directory.path().join("oversized.txt"))
        .is_err());
}

#[cfg(unix)]
#[test]
fn saving_follows_symlinks_and_preserves_permissions() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("源码.txt");
    fs::write(&target, b"original").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
    let link = directory.path().join("link.txt");
    symlink(&target, &link).unwrap();
    let mut original = document(link.clone());
    original.text = "new\n".into();
    NativeTextBackend.save(&original).unwrap();
    assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert_eq!(NativeTextBackend.load(&target).unwrap().text, "new\n");
}

// Linux filesystems permit arbitrary non-NUL bytes in names; macOS rejects
// malformed UTF-8 before a backend can open the file.
#[cfg(target_os = "linux")]
#[test]
fn native_paths_support_non_utf8_filenames() {
    use std::os::unix::ffi::OsStringExt;
    let directory = tempfile::tempdir().unwrap();
    let path = directory
        .path()
        .join(std::ffi::OsString::from_vec(b"source-\xff.txt".to_vec()));
    let original = document(path);
    NativeTextBackend.save(&original).unwrap();
    assert_eq!(
        NativeTextBackend.load(&original.path).unwrap().text,
        original.text
    );
}
