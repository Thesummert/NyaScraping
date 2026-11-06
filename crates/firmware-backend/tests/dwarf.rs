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

fn synthetic(
    version: u16,
    endian: RunTimeEndian,
    compressed: bool,
    invalid_locations: bool,
) -> Vec<u8> {
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
    line.begin_sequence(Some(write::Address::Constant(0x08001200)));
    line.row().file = file;
    line.row().line = 10;
    line.generate_row();
    line.row().address_offset = 4;
    line.generate_row(); // Multiple addresses for one source line.
    line.row().line = 12;
    line.row().address_offset = 8;
    line.generate_row();
    line.row().line = 99; // The end-sequence row is not an executable line.
    line.end_sequence(12);
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
    let extern_var = declarations.add(root, gimli::DW_TAG_variable);
    declarations
        .get_mut(extern_var)
        .set(gimli::DW_AT_name, A::String(b"extern_counter".to_vec()));
    declarations
        .get_mut(extern_var)
        .set(gimli::DW_AT_type, A::UnitRef(int));
    declarations
        .get_mut(extern_var)
        .set(gimli::DW_AT_declaration, A::Flag(true));
    let point = declarations.add(root, gimli::DW_TAG_structure_type);
    declarations
        .get_mut(point)
        .set(gimli::DW_AT_name, A::String(b"Point".to_vec()));
    let x = declarations.add(point, gimli::DW_TAG_member);
    declarations
        .get_mut(x)
        .set(gimli::DW_AT_name, A::String(b"x".to_vec()));
    declarations
        .get_mut(x)
        .set(gimli::DW_AT_type, A::UnitRef(int));
    declarations
        .get_mut(x)
        .set(gimli::DW_AT_data_member_location, A::Udata(8));
    let bits = declarations.add(point, gimli::DW_TAG_member);
    declarations
        .get_mut(bits)
        .set(gimli::DW_AT_name, A::String(b"bits".to_vec()));
    declarations
        .get_mut(bits)
        .set(gimli::DW_AT_type, A::UnitRef(int));
    declarations
        .get_mut(bits)
        .set(gimli::DW_AT_data_member_location, A::Udata(0));
    declarations
        .get_mut(bits)
        .set(gimli::DW_AT_bit_size, A::Udata(1));
    let state = declarations.add(root, gimli::DW_TAG_structure_type);
    declarations
        .get_mut(state)
        .set(gimli::DW_AT_name, A::String(b"State".to_vec()));
    let position = declarations.add(state, gimli::DW_TAG_member);
    declarations
        .get_mut(position)
        .set(gimli::DW_AT_name, A::String(b"position".to_vec()));
    declarations
        .get_mut(position)
        .set(gimli::DW_AT_type, A::UnitRef(point));
    declarations
        .get_mut(position)
        .set(gimli::DW_AT_data_member_location, A::Udata(4));
    let state_ptr = declarations.add(root, gimli::DW_TAG_pointer_type);
    declarations
        .get_mut(state_ptr)
        .set(gimli::DW_AT_type, A::UnitRef(state));
    let next = declarations.add(state, gimli::DW_TAG_member);
    declarations
        .get_mut(next)
        .set(gimli::DW_AT_name, A::String(b"next".to_vec()));
    declarations
        .get_mut(next)
        .set(gimli::DW_AT_type, A::UnitRef(state_ptr));
    let alias = declarations.add(root, gimli::DW_TAG_typedef);
    declarations
        .get_mut(alias)
        .set(gimli::DW_AT_name, A::String(b"StateAlias".to_vec()));
    declarations
        .get_mut(alias)
        .set(gimli::DW_AT_type, A::UnitRef(state));
    let qualified = declarations.add(root, gimli::DW_TAG_const_type);
    declarations
        .get_mut(qualified)
        .set(gimli::DW_AT_type, A::UnitRef(alias));
    let state_var = declarations.add(root, gimli::DW_TAG_variable);
    declarations
        .get_mut(state_var)
        .set(gimli::DW_AT_name, A::String(b"state".to_vec()));
    declarations
        .get_mut(state_var)
        .set(gimli::DW_AT_type, A::UnitRef(qualified));
    let array = declarations.add(root, gimli::DW_TAG_array_type);
    declarations
        .get_mut(array)
        .set(gimli::DW_AT_type, A::UnitRef(int));
    for count in [2, 3] {
        let dim = declarations.add(array, gimli::DW_TAG_subrange_type);
        declarations
            .get_mut(dim)
            .set(gimli::DW_AT_count, A::Udata(count));
    }
    let matrix = declarations.add(root, gimli::DW_TAG_variable);
    declarations
        .get_mut(matrix)
        .set(gimli::DW_AT_name, A::String(b"matrix".to_vec()));
    declarations
        .get_mut(matrix)
        .set(gimli::DW_AT_type, A::UnitRef(array));
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
    // A reader-backed operation must display its own bytes, not this large
    // shared .debug_info buffer. This also covers compressed debug sections.
    declarations
        .get_mut(root)
        .set(gimli::DW_AT_producer, A::String(vec![b'x'; 8192]));
    let mut register = write::Expression::new();
    register.op_reg(gimli::Register(0));
    let mut entry = write::Expression::new();
    entry.op_entry_value(register);
    let mut implicit = write::Expression::new();
    implicit.op_implicit_value(vec![0x12, 0x34].into_boxed_slice());
    let mut typed = write::Expression::new();
    typed.op_const_type(int, vec![0x56, 0x78].into_boxed_slice());
    let mut large = write::Expression::new();
    large.op_implicit_value(vec![0xa5; 256].into_boxed_slice());
    for (name, expression) in [
        ("entry_value", entry),
        ("implicit_value", implicit),
        ("typed_literal", typed),
        ("large_implicit", large),
    ] {
        let variable = declarations.add(root, gimli::DW_TAG_variable);
        declarations
            .get_mut(variable)
            .set(gimli::DW_AT_name, A::String(name.as_bytes().to_vec()));
        declarations
            .get_mut(variable)
            .set(gimli::DW_AT_type, A::UnitRef(int));
        declarations
            .get_mut(variable)
            .set(gimli::DW_AT_location, A::Exprloc(expression));
    }
    if invalid_locations {
        for (name, in_list, bytecode) in [
            ("unsupported_inline", false, vec![0xff]),
            ("truncated_inline", false, vec![gimli::DW_OP_addr.0]),
            ("unsupported_list", true, vec![0xff]),
            ("truncated_list", true, vec![gimli::DW_OP_addr.0]),
        ] {
            let expression = write::Expression::raw(bytecode);
            let location = if in_list {
                let mut valid = write::Expression::new();
                valid.op_reg(gimli::Register(0));
                let list = declarations.locations.add(write::LocationList(vec![
                    write::Location::StartLength {
                        begin: write::Address::Constant(0x08001200),
                        length: 4,
                        data: valid,
                    },
                    write::Location::StartLength {
                        begin: write::Address::Constant(0x08001204),
                        length: 4,
                        data: expression,
                    },
                ]));
                A::LocationListRef(list)
            } else {
                A::Exprloc(expression)
            };
            let variable = declarations.add(
                if in_list { function } else { root },
                gimli::DW_TAG_variable,
            );
            for (attribute, value) in [
                (gimli::DW_AT_name, A::String(name.as_bytes().to_vec())),
                (gimli::DW_AT_type, A::UnitRef(int)),
                (gimli::DW_AT_decl_file, A::FileIndex(Some(file))),
                (gimli::DW_AT_decl_line, A::Udata(12)),
                (gimli::DW_AT_location, location),
            ] {
                declarations.get_mut(variable).set(attribute, value);
            }
        }
    }
    let mut dwarf = write::Dwarf::new();
    let unit_id = dwarf.units.add(declarations);
    let mut definitions = write::Unit::new(encoding, write::LineProgram::none());
    let extern_definition = definitions.add(definitions.root(), gimli::DW_TAG_variable);
    definitions.get_mut(extern_definition).set(
        gimli::DW_AT_specification,
        A::DebugInfoRef(write::Reference::Entry(unit_id, extern_var)),
    );
    let mut extern_address = write::Expression::new();
    extern_address.op_addr(write::Address::Constant(0x20000300));
    definitions
        .get_mut(extern_definition)
        .set(gimli::DW_AT_location, A::Exprloc(extern_address));
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
                &synthetic(version, endian, compressed, false),
            )
            .unwrap();
        let info = image.debug;
        assert!(info.warnings.is_empty(), "{:?}", info.warnings);
        assert_eq!(info.files[0].path, "C:/project/src/main.cpp");
        let lines: Vec<_> = info
            .executable_lines
            .iter()
            .map(|loc| (info.files[loc.file].path.as_str(), loc.line.unwrap()))
            .collect();
        assert_eq!(
            lines,
            vec![
                ("C:/project/src/main.cpp", 10),
                ("C:/project/src/main.cpp", 12)
            ]
        );
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
        let counters: Vec<_> = info
            .variables
            .iter()
            .filter(|v| v.name == "extern_counter")
            .collect();
        assert_eq!(counters.len(), 1);
        assert!(matches!(
            counters[0].location,
            VariableLocation::Address(0x20000300)
        ));
        assert_eq!(counters[0].scalar_type, Some(ScalarType::Signed));
        assert_eq!(counters[0].byte_size, Some(4));
        let matrix = info.variables.iter().find(|v| v.name == "matrix").unwrap();
        assert_eq!(matrix.type_name.as_deref(), Some("int[2][3]"));
        assert_eq!(matrix.members.len(), 2);
        assert_eq!(matrix.members[1].offset, Some(12));
        assert_eq!(matrix.members[1].type_name, "int[3]");
        assert_eq!(matrix.members[1].members[2].name, "[2]");
        assert_eq!(matrix.members[1].members[2].offset, Some(8));
        assert_eq!(
            matrix.members[1].members[2].scalar_type,
            Some(ScalarType::Signed)
        );
        let state = info.variables.iter().find(|v| v.name == "state").unwrap();
        assert_eq!(state.type_name.as_deref(), Some("const StateAlias"));
        assert_eq!(state.members.len(), 2);
        assert_eq!(state.members[0].name, "position");
        assert_eq!(state.members[0].type_name, "Point");
        assert_eq!(state.members[0].members[0].name, "x");
        assert_eq!(state.members[0].members[0].type_name, "int");
        assert_eq!(state.members[0].offset, Some(4));
        assert_eq!(state.members[0].members[0].offset, Some(8));
        assert_eq!(
            state.members[0].members[0].scalar_type,
            Some(ScalarType::Signed)
        );
        assert_eq!(state.members[0].members[0].byte_size, Some(4));
        assert_eq!(state.members[0].members[1].offset, None);
        assert_eq!(state.members[0].members[1].scalar_type, None);
        assert_eq!(state.members[1].name, "next");
        assert_eq!(state.members[1].type_name, "State *");
        assert!(state.members[1].members.is_empty());
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
fn invalid_variable_locations_preserve_source_types_functions_and_other_variables() {
    for version in [4, 5] {
        let image = NativeFirmwareBackend
            .parse(
                Path::new("invalid-locations.elf"),
                &synthetic(version, RunTimeEndian::Little, false, true),
            )
            .unwrap();
        let info = image.debug;
        assert_eq!(info.warnings.len(), 4, "{:?}", info.warnings);
        assert!(!info.executable_lines.is_empty());
        let function = info
            .functions
            .iter()
            .find(|f| f.name == "calculate" && !f.ranges.is_empty())
            .unwrap();
        let declaration = function.declaration.as_ref().unwrap();
        assert_eq!(info.files[declaration.file].path, "C:/project/src/main.cpp");
        assert_eq!(declaration.line, Some(27));
        for name in [
            "unsupported_inline",
            "truncated_inline",
            "unsupported_list",
            "truncated_list",
        ] {
            let variable = info.variables.iter().find(|v| v.name == name).unwrap();
            assert!(matches!(variable.location, VariableLocation::Unavailable));
            assert_eq!(variable.type_name.as_deref(), Some("int"));
            assert_eq!(variable.byte_size, Some(4));
            assert_eq!(variable.scalar_type, Some(ScalarType::Signed));
            let declaration = variable.declaration.as_ref().unwrap();
            assert_eq!(info.files[declaration.file].path, "C:/project/src/main.cpp");
            assert_eq!(declaration.line, Some(12));
            assert_eq!(variable.function.is_some(), name.ends_with("_list"));
            assert!(info.warnings.iter().any(|warning| warning.contains(name)));
        }
        let healthy = info.variables.iter().find(|v| v.name == "voltage").unwrap();
        assert!(matches!(
            healthy.location,
            VariableLocation::Address(0x20000200)
        ));
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
    assert!(info
        .executable_lines
        .iter()
        .any(|loc| info.files[loc.file].path == "/firmware/source/main.c" && loc.line == Some(9)));
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
    assert_eq!(sensor.members.len(), 2);
    assert_eq!(sensor.members[0].name, "value");
    assert_eq!(sensor.members[0].type_name, "int");
    assert_eq!(sensor.members[1].name, "label");
    assert_eq!(sensor.members[1].type_name, "const char *");
    assert!(sensor.members[1].members.is_empty());
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

#[test]
fn expression_previews_are_bounded_to_the_active_bytes_and_progress_is_monotonic() {
    for (version, endian, compressed) in [
        (4, RunTimeEndian::Little, false),
        (5, RunTimeEndian::Big, true),
    ] {
        let mut progress = Vec::new();
        let image = NativeFirmwareBackend
            .parse_with_progress(
                Path::new("expressions.elf"),
                &synthetic(version, endian, compressed, false),
                &mut |value| progress.push(value),
            )
            .unwrap();
        assert!(
            image.debug.warnings.is_empty(),
            "{:?}",
            image.debug.warnings
        );
        for (name, operation, payload) in [
            ("entry_value", "EntryValue", "[50]"),
            ("implicit_value", "ImplicitValue", "[12, 34]"),
            ("typed_literal", "TypedLiteral", "[56, 78]"),
            ("large_implicit", "ImplicitValue", "256 bytes"),
        ] {
            let variable = image
                .debug
                .variables
                .iter()
                .find(|v| v.name == name)
                .unwrap();
            let VariableLocation::Expression(text) = &variable.location else {
                panic!("expected expression for {name}");
            };
            assert!(text.contains(operation) && text.contains(payload), "{text}");
            assert!(
                text.len() < 400,
                "{name} must not dump its backing DWARF section"
            );
        }
        assert!(progress.iter().all(|p| (0.0..=1.0).contains(&p.fraction)));
        assert!(progress.windows(2).all(|p| p[0].fraction <= p[1].fraction));
        assert!(progress
            .iter()
            .any(|p| p.stage == "解析变量、类型与成员" && p.total > 0));
        assert_eq!(progress.last().unwrap().stage, "整理符号与调试信息");
    }
}
