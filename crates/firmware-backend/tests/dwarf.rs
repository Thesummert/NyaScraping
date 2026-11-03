use nya_domain::firmware::{VariableKind, VariableLocation};
use nya_domain::waveform::ScalarType;
use nya_firmware_backend::NativeFirmwareBackend;
use std::path::Path;

use gimli::{
    write::{self, AttributeValue as A},
    Encoding, Format, RunTimeEndian,
};
use object::{Architecture, BinaryFormat, Endianness, SectionKind};
use std::io::Write;

fn synthetic(version: u16, endian: RunTimeEndian, compressed: bool) -> Vec<u8> {
    let encoding = Encoding {
        format: Format::Dwarf32,
        version,
        address_size: 8,
    };
    let mut line = write::LineProgram::new(
        encoding,
        Default::default(),
        write::LineString::String(b"C:\\project\\src".to_vec()),
        write::LineString::String(b"main.cpp".to_vec()),
        None,
    );
    let file = line.add_file(
        write::LineString::String(b"main.cpp".to_vec()),
        line.default_directory(),
        None,
    );
    let mut declarations = write::Unit::new(encoding, line);
    let root = declarations.root();
    declarations
        .get_mut(root)
        .set(gimli::DW_AT_name, A::String(b"main.cpp".to_vec()));
    declarations.get_mut(root).set(
        gimli::DW_AT_comp_dir,
        A::String(b"C:\\project\\src".to_vec()),
    );
    declarations
        .get_mut(root)
        .set(gimli::DW_AT_stmt_list, A::LineProgramRef);
    let int = declarations.add(root, gimli::DW_TAG_base_type);
    declarations
        .get_mut(int)
        .set(gimli::DW_AT_name, A::String(b"int".to_vec()));
    declarations
        .get_mut(int)
        .set(gimli::DW_AT_byte_size, A::Udata(4));
    declarations
        .get_mut(int)
        .set(gimli::DW_AT_encoding, A::Encoding(gimli::DW_ATE_signed));
    let function = declarations.add(root, gimli::DW_TAG_subprogram);
    for (attribute, value) in [
        (gimli::DW_AT_name, A::String(b"calculate".to_vec())),
        (
            gimli::DW_AT_linkage_name,
            A::String(b"_Z9calculatei".to_vec()),
        ),
        (gimli::DW_AT_decl_file, A::FileIndex(Some(file))),
        (gimli::DW_AT_decl_line, A::Udata(27)),
        (gimli::DW_AT_decl_column, A::Udata(3)),
        (gimli::DW_AT_declaration, A::Flag(true)),
        (gimli::DW_AT_type, A::UnitRef(int)),
    ] {
        declarations.get_mut(function).set(attribute, value);
    }
    let mut dwarf = write::Dwarf::new();
    let unit_id = dwarf.units.add(declarations);
    let mut definitions = write::Unit::new(encoding, write::LineProgram::none());
    let concrete = definitions.add(definitions.root(), gimli::DW_TAG_subprogram);
    definitions.get_mut(concrete).set(
        gimli::DW_AT_specification,
        A::DebugInfoRef(write::Reference::Entry(unit_id, function)),
    );
    definitions.get_mut(concrete).set(
        gimli::DW_AT_low_pc,
        A::Address(write::Address::Constant(0x08001200)),
    );
    definitions
        .get_mut(concrete)
        .set(gimli::DW_AT_high_pc, A::Udata(32));
    let parameter = definitions.add(concrete, gimli::DW_TAG_formal_parameter);
    definitions
        .get_mut(parameter)
        .set(gimli::DW_AT_name, A::String(b"input".to_vec()));
    definitions.get_mut(parameter).set(
        gimli::DW_AT_type,
        A::DebugInfoRef(write::Reference::Entry(unit_id, int)),
    );
    definitions
        .get_mut(parameter)
        .set(gimli::DW_AT_const_value, A::Sdata(-7));
    let pointer = definitions.add(definitions.root(), gimli::DW_TAG_pointer_type);
    definitions.get_mut(pointer).set(
        gimli::DW_AT_type,
        A::DebugInfoRef(write::Reference::Entry(unit_id, int)),
    );
    let global = definitions.add(definitions.root(), gimli::DW_TAG_variable);
    definitions
        .get_mut(global)
        .set(gimli::DW_AT_name, A::String(b"global_ptr".to_vec()));
    definitions
        .get_mut(global)
        .set(gimli::DW_AT_type, A::UnitRef(pointer));
    let mut address = write::Expression::new();
    address.op_addr(write::Address::Constant(0x20000100));
    definitions
        .get_mut(global)
        .set(gimli::DW_AT_location, A::Exprloc(address));
    for (index, (name, encoding, size)) in [
        ("voltage", gimli::DW_ATE_float, 4),
        ("flags", gimli::DW_ATE_unsigned, 8),
        ("ready", gimli::DW_ATE_boolean, 1),
        ("character", gimli::DW_ATE_signed_char, 1),
    ]
    .into_iter()
    .enumerate()
    {
        let base = definitions.add(definitions.root(), gimli::DW_TAG_base_type);
        definitions.get_mut(base).set(
            gimli::DW_AT_name,
            A::String(format!("base_{name}").into_bytes()),
        );
        definitions
            .get_mut(base)
            .set(gimli::DW_AT_byte_size, A::Udata(size));
        definitions
            .get_mut(base)
            .set(gimli::DW_AT_encoding, A::Encoding(encoding));
        let alias = definitions.add(definitions.root(), gimli::DW_TAG_typedef);
        definitions.get_mut(alias).set(
            gimli::DW_AT_name,
            A::String(format!("Alias_{name}").into_bytes()),
        );
        definitions
            .get_mut(alias)
            .set(gimli::DW_AT_type, A::UnitRef(base));
        let qualified = definitions.add(definitions.root(), gimli::DW_TAG_volatile_type);
        definitions
            .get_mut(qualified)
            .set(gimli::DW_AT_type, A::UnitRef(alias));
        let variable = definitions.add(definitions.root(), gimli::DW_TAG_variable);
        definitions
            .get_mut(variable)
            .set(gimli::DW_AT_name, A::String(name.as_bytes().to_vec()));
        definitions
            .get_mut(variable)
            .set(gimli::DW_AT_type, A::UnitRef(qualified));
        let mut location = write::Expression::new();
        location.op_addr(write::Address::Constant(0x20000200 + index as u64 * 8));
        definitions
            .get_mut(variable)
            .set(gimli::DW_AT_location, A::Exprloc(location));
    }
    let cycle = definitions.add(definitions.root(), gimli::DW_TAG_const_type);
    definitions
        .get_mut(cycle)
        .set(gimli::DW_AT_type, A::UnitRef(cycle));
    let recursive = definitions.add(definitions.root(), gimli::DW_TAG_variable);
    definitions
        .get_mut(recursive)
        .set(gimli::DW_AT_name, A::String(b"recursive".to_vec()));
    definitions
        .get_mut(recursive)
        .set(gimli::DW_AT_type, A::UnitRef(cycle));
    dwarf.units.add(definitions);
    let mut sections = write::Sections::new(write::EndianVec::new(endian));
    dwarf.write(&mut sections).unwrap();
    let mut object = object::write::Object::new(
        BinaryFormat::Elf,
        Architecture::Aarch64,
        if endian == RunTimeEndian::Little {
            Endianness::Little
        } else {
            Endianness::Big
        },
    );
    sections
        .for_each(|id, data| -> Result<(), ()> {
            if data.slice().is_empty() {
                return Ok(());
            }
            let mut name = id.name().to_string();
            let mut bytes = data.slice().to_vec();
            if compressed && id == gimli::SectionId::DebugInfo {
                name = ".zdebug_info".into();
                let mut encoder =
                    flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
                encoder.write_all(&bytes).unwrap();
                let mut compressed = b"ZLIB".to_vec();
                compressed.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
                compressed.extend_from_slice(&encoder.finish().unwrap());
                bytes = compressed;
            }
            let section = object.add_section(Vec::new(), name.into_bytes(), SectionKind::Debug);
            object.append_section_data(section, &bytes, 1);
            Ok(())
        })
        .unwrap();
    object.write().unwrap()
}

#[test]
fn cross_unit_specifications_types_constants_big_endian_and_compressed_dwarf() {
    for (version, endian, compressed) in [
        (4, RunTimeEndian::Little, false),
        (5, RunTimeEndian::Big, true),
    ] {
        let image = NativeFirmwareBackend
            .parse(
                Path::new("synthetic.axf"),
                &synthetic(version, endian, compressed),
            )
            .unwrap();
        let info = image.debug;
        assert!(info.warnings.is_empty(), "{:?}", info.warnings);
        assert_eq!(info.files[0].path, "C:/project/src/main.cpp");
        let concrete = info
            .functions
            .iter()
            .find(|f| !f.ranges.is_empty())
            .unwrap();
        assert_eq!(concrete.name, "calculate");
        assert_eq!(concrete.linkage_name.as_deref(), Some("_Z9calculatei"));
        assert_eq!(concrete.return_type.as_deref(), Some("int"));
        assert_eq!(concrete.declaration.as_ref().unwrap().line, Some(27));
        assert_eq!(concrete.declaration.as_ref().unwrap().column, Some(3));
        assert!(!concrete.declaration_only);
        assert_eq!(concrete.ranges[0].begin, 0x08001200);
        assert_eq!(concrete.ranges[0].end, 0x08001220);
        let parameter = info.variables.iter().find(|v| v.name == "input").unwrap();
        assert_eq!(parameter.constant.as_deref(), Some("-7"));
        assert_eq!(parameter.type_name.as_deref(), Some("int"));
        assert_eq!(parameter.scalar_type, Some(ScalarType::Signed));
        assert!(parameter.function.is_some());
        assert!(matches!(parameter.location, VariableLocation::Unavailable));
        let global = info
            .variables
            .iter()
            .find(|v| v.name == "global_ptr")
            .unwrap();
        assert_eq!(global.type_name.as_deref(), Some("int *"));
        assert_eq!(global.byte_size, Some(8));
        assert_eq!(global.scalar_type, None);
        for (name, scalar_type, size) in [
            ("voltage", ScalarType::Float, 4),
            ("flags", ScalarType::Unsigned, 8),
            ("ready", ScalarType::Boolean, 1),
            ("character", ScalarType::Signed, 1),
        ] {
            let variable = info.variables.iter().find(|v| v.name == name).unwrap();
            assert_eq!(variable.scalar_type, Some(scalar_type));
            assert_eq!(variable.byte_size, Some(size));
            assert_eq!(
                variable.type_name.as_ref().unwrap(),
                &format!("volatile Alias_{name}")
            );
        }
        assert!(matches!(
            global.location,
            VariableLocation::Address(0x20000100)
        ));
        assert!(info
            .variables
            .iter()
            .find(|v| v.name == "recursive")
            .unwrap()
            .type_name
            .as_ref()
            .unwrap()
            .contains("递归"));
    }
}

#[test]
fn clang_arm_dwarf4_files_functions_types_and_lexical_variables() {
    let image = NativeFirmwareBackend
        .parse(
            Path::new("arm.axf"),
            include_bytes!("fixtures/arm-dwarf4.axf"),
        )
        .unwrap();
    let info = image.debug;
    assert!(info.warnings.is_empty(), "{:?}", info.warnings);
    assert!(
        info.files
            .iter()
            .any(|file| file.path == "/firmware/source/main.c"),
        "{:?}",
        info.files
    );
    assert!(
        info.files
            .iter()
            .any(|file| file.path == "/firmware/source/types.h"),
        "{:?}",
        info.files
    );
    let compute_index = info
        .functions
        .iter()
        .position(|f| f.name == "compute")
        .unwrap();
    let compute = &info.functions[compute_index];
    assert_eq!(compute.return_type.as_deref(), Some("int"));
    let declaration = compute.declaration.as_ref().unwrap();
    assert_eq!(declaration.line, Some(8));
    assert!(info.files[declaration.file].path.ends_with("main.c"));
    assert!(!compute.ranges.is_empty());
    for (name, kind) in [
        ("input", VariableKind::Parameter),
        ("local", VariableKind::Local),
        ("nested", VariableKind::Local),
    ] {
        let variable = info
            .variables
            .iter()
            .find(|v| v.name == name && v.function == Some(compute_index))
            .unwrap();
        assert_eq!(variable.kind, kind);
        assert_eq!(variable.type_name.as_deref(), Some("int"));
        assert_eq!(variable.byte_size, Some(4));
        assert!(matches!(variable.location, VariableLocation::Expression(_)));
        assert!(variable.declaration.as_ref().unwrap().line.is_some());
    }
    let global = info
        .variables
        .iter()
        .find(|v| v.name == "global_counter")
        .unwrap();
    assert_eq!(global.kind, VariableKind::Global);
    assert_eq!(global.type_name.as_deref(), Some("volatile int"));
    assert_eq!(global.scalar_type, Some(ScalarType::Signed));
    assert!(matches!(global.location, VariableLocation::Address(_)));
    let samples = info.variables.iter().find(|v| v.name == "samples").unwrap();
    assert_eq!(samples.type_name.as_deref(), Some("int[4]"));
    assert_eq!(samples.byte_size, Some(16));
    assert_eq!(samples.scalar_type, None);
    let header = info
        .variables
        .iter()
        .find(|v| v.name == "header_counter")
        .unwrap();
    assert!(info.files[header.declaration.as_ref().unwrap().file]
        .path
        .ends_with("types.h"));
    let sensor = info.variables.iter().find(|v| v.name == "sensor").unwrap();
    assert_eq!(sensor.type_name.as_deref(), Some("Sensor"));
    assert_eq!(sensor.byte_size, Some(8));
    assert_eq!(sensor.scalar_type, None);
}

#[test]
fn clang_dwarf5_optimized_inlining_indexed_strings_addresses_and_locations() {
    let image = NativeFirmwareBackend
        .parse(
            Path::new("optimized.axf"),
            include_bytes!("fixtures/arm-dwarf5.axf"),
        )
        .unwrap();
    let info = image.debug;
    assert!(info.warnings.is_empty(), "{:?}", info.warnings);
    assert!(info.functions.iter().any(|f| f.name == "compute"));
    assert!(info
        .functions
        .iter()
        .any(|f| f.name == "scale" && f.inlined));
    assert!(
        info.variables
            .iter()
            .any(|v| matches!(v.location, VariableLocation::List(_))),
        "{:?}",
        info.variables
    );
    assert!(info
        .variables
        .iter()
        .filter(|v| v.kind == VariableKind::Parameter)
        .all(|v| v.name == "input"));
    let global = info
        .variables
        .iter()
        .find(|v| v.name == "global_counter")
        .unwrap();
    assert!(matches!(global.location, VariableLocation::Address(_)));
    assert!(global.declaration.is_some());
    assert!(info.files.iter().all(|f| !f.path.contains("/Users/")));
}
