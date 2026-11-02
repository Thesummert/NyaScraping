use nya_domain::firmware::FirmwareBackend;
use nya_firmware_backend::{NativeFirmwareBackend, MAX_FILE_BYTES};
use object::{
    write::{Object, StandardSection, Symbol, SymbolSection},
    Architecture, BinaryFormat, Endianness, SectionKind, SymbolFlags, SymbolKind, SymbolScope,
};
use std::{fs, path::Path};

fn fixture(format: BinaryFormat, architecture: Architecture, endian: Endianness) -> Vec<u8> {
    let mut object = Object::new(format, architecture, endian);
    let text = object.section_id(StandardSection::Text);
    object.append_section_data(text, &[0x00, 0xbf, 0x70, 0x47], 4);
    let data = object.section_id(StandardSection::Data);
    object.append_section_data(data, &[42, 0, 0, 0], 4);
    let bss = object.section_id(StandardSection::UninitializedData);
    object.append_section_bss(bss, 64, 4);
    let debug = object.add_section(Vec::new(), b".debug_info".to_vec(), SectionKind::Debug);
    object.append_section_data(debug, &[1, 2, 3, 4], 1);
    for (name, kind, section) in [
        ("main", SymbolKind::Text, text),
        ("counter", SymbolKind::Data, data),
    ] {
        object.add_symbol(Symbol {
            name: name.as_bytes().to_vec(),
            value: 0,
            size: 4,
            kind,
            scope: SymbolScope::Linkage,
            weak: false,
            section: SymbolSection::Section(section),
            flags: SymbolFlags::None,
        });
    }
    object.add_symbol(Symbol {
        name: b"external".to_vec(),
        value: 0,
        size: 0,
        kind: SymbolKind::Text,
        scope: SymbolScope::Unknown,
        weak: true,
        section: SymbolSection::Undefined,
        flags: SymbolFlags::None,
    });
    object.write().unwrap()
}

#[test]
fn elf_classes_endianness_sections_and_symbols() {
    for (architecture, endian, bits) in [
        (Architecture::Arm, Endianness::Little, 32),
        (Architecture::Arm, Endianness::Big, 32),
        (Architecture::X86_64, Endianness::Little, 64),
        (Architecture::Aarch64, Endianness::Big, 64),
    ] {
        let bytes = fixture(BinaryFormat::Elf, architecture, endian);
        let image = NativeFirmwareBackend
            .parse(Path::new("firmware.axf"), &bytes)
            .unwrap();
        assert_eq!(image.format, "Elf");
        assert_eq!(image.address_bits, bits);
        assert_eq!(image.little_endian, endian == Endianness::Little);
        assert!(image.has_debug_info);
        assert_eq!(image.kind, "Relocatable");
        let bss = image.sections.iter().find(|s| s.name == ".bss").unwrap();
        assert_eq!(bss.size, 64);
        assert_eq!(bss.file_range, None);
        let main = image.symbols.iter().find(|s| s.name == "main").unwrap();
        assert_eq!(main.size, 4);
        assert_eq!(main.kind, "Text");
        assert!(!main.undefined);
        assert!(!main.dynamic);
        assert!(main.section_index.is_some());
        let external = image.symbols.iter().find(|s| s.name == "external").unwrap();
        assert!(external.undefined && external.weak);
        assert_eq!(external.section_index, None);
    }
}

// A minimal stripped ARM executable with one PT_LOAD and no section table.
fn executable() -> Vec<u8> {
    let mut bytes = vec![0; 88];
    bytes[..16].copy_from_slice(&[0x7f, b'E', b'L', b'F', 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    for (offset, value) in [(16, 2u16), (18, 40), (40, 52), (42, 32), (44, 1), (46, 40)] {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }
    for (offset, value) in [
        (20, 1u32),
        (24, 0x08000001),
        (28, 52),
        (52, 1),
        (56, 84),
        (60, 0x08000000),
        (64, 0x08000000),
        (68, 4),
        (72, 16),
        (76, 5),
        (80, 4),
    ] {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    bytes[84..].copy_from_slice(&[0, 0xbf, 0x70, 0x47]);
    bytes
}

#[test]
fn stripped_executable_entry_and_load_segment() {
    let image = NativeFirmwareBackend
        .parse(Path::new("stripped.out"), &executable())
        .unwrap();
    assert_eq!(image.entry, 0x08000001);
    assert_eq!(image.kind, "Executable");
    assert!(!image.has_debug_info);
    assert!(image.symbols.is_empty());
    assert_eq!(image.segments.len(), 1);
    let segment = &image.segments[0];
    assert_eq!(segment.address, 0x08000000);
    assert_eq!(segment.memory_size, 16);
    assert_eq!((segment.file_offset, segment.file_size), (84, 4));
}

#[test]
fn other_object_formats() {
    for (format, expected) in [(BinaryFormat::Coff, "Coff"), (BinaryFormat::MachO, "MachO")] {
        let bytes = fixture(format, Architecture::X86_64, Endianness::Little);
        let image = NativeFirmwareBackend
            .parse(Path::new("image.o"), &bytes)
            .unwrap();
        assert_eq!(image.format, expected);
        let main = if format == BinaryFormat::MachO {
            "_main"
        } else {
            "main"
        };
        assert!(image.symbols.iter().any(|s| s.name == main));
    }
}

#[test]
fn malformed_dwarf_keeps_object_metadata_and_symbol_fallback() {
    let bytes = fixture(BinaryFormat::Elf, Architecture::Arm, Endianness::Little);
    let image = NativeFirmwareBackend
        .parse(Path::new("broken-debug.axf"), &bytes)
        .unwrap();
    assert!(image
        .debug
        .warnings
        .iter()
        .any(|w| w.contains("DWARF 解析失败")));
    assert!(image.debug.files.is_empty());
    assert!(image
        .debug
        .functions
        .iter()
        .any(|f| f.name == "main" && f.declaration.is_none()));
    assert!(image
        .debug
        .variables
        .iter()
        .any(|v| v.name == "counter" && v.type_name.is_none()));
    assert!(image.sections.iter().any(|s| s.name == ".text"));
}

#[test]
fn oversized_compressed_dwarf_is_rejected_before_decompression() {
    let mut object = Object::new(BinaryFormat::Elf, Architecture::Arm, Endianness::Little);
    let section = object.add_section(Vec::new(), b".zdebug_info".to_vec(), SectionKind::Debug);
    let mut bytes = b"ZLIB".to_vec();
    bytes.extend_from_slice(&(MAX_FILE_BYTES + 1).to_be_bytes());
    bytes.extend_from_slice(&[0x78, 0x9c, 0, 0]);
    object.append_section_data(section, &bytes, 1);
    let bytes = object.write().unwrap();
    let image = NativeFirmwareBackend
        .parse(Path::new("large-debug.axf"), &bytes)
        .unwrap();
    assert!(
        image
            .debug
            .warnings
            .iter()
            .any(|w| w.contains("decompressed DWARF exceeds")),
        "{:?}",
        image.debug.warnings
    );
}

#[test]
fn malformed_and_truncated_inputs_are_errors() {
    for bytes in [
        &b""[..],
        &b"not an object file"[..],
        &b"\x7fELF"[..],
        &b":00000001FF"[..],
    ] {
        assert!(NativeFirmwareBackend
            .parse(Path::new("bad.elf"), bytes)
            .is_err());
    }
    let bytes = executable();
    // Header truncation, then a segment that points past the end of the file.
    for size in [16, 40, 52, 70, 84, 87] {
        assert!(NativeFirmwareBackend
            .parse(Path::new("bad.axf"), &bytes[..size])
            .is_err());
    }
    let mut bad = bytes;
    bad[56..60].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(NativeFirmwareBackend
        .parse(Path::new("bad.axf"), &bad)
        .is_err());
}

#[test]
fn unicode_paths_content_detection_and_file_limits() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("固件.efl");
    fs::write(&path, executable()).unwrap();
    let image = NativeFirmwareBackend.load(&path).unwrap();
    assert_eq!(image.path, path);
    assert_eq!(image.format, "Elf");
    assert!(NativeFirmwareBackend.load(dir.path()).is_err());
    assert!(NativeFirmwareBackend
        .load(&dir.path().join("missing.axf"))
        .is_err());
    let large = fs::File::create(dir.path().join("large.elf")).unwrap();
    large.set_len(MAX_FILE_BYTES + 1).unwrap();
    assert!(NativeFirmwareBackend
        .load(&dir.path().join("large.elf"))
        .is_err());
}
