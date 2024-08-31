use gimli::write::{
    Address, AttributeValue, DebuggingInformationEntry, Dwarf, DwarfUnit, EndianVec, Expression,
    LineProgram, LineString, Location, LocationList, Range, RangeList, Sections, Unit,
};
use gimli::{Attribute, DW_TAG_base_type, DW_TAG_subprogram, LineEncoding};
use goblin::elf64::{
    header::*, program_header as segment, section_header as section, sym as symbol,
};
use scroll::{Pread, Pwrite};
use std::collections::BTreeMap as HashMap;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::mem::transmute;
use std::path::Path;

use serde::{Deserialize, Serialize};

type RawSection = section::SectionHeader;
type RawSegment = segment::ProgramHeader;
type RawSymbol = symbol::Sym;
const SIZEOF_SHDR: usize = section::SIZEOF_SHDR;
const SIZEOF_PHDR: usize = segment::SIZEOF_PHDR;
const SIZEOF_SYM: usize = symbol::SIZEOF_SYM;

struct Section {
    hdr: RawSection,
    raw: Vec<u8>,
    off: u64,
}

struct Segment {
    hdr: RawSegment,
    raw: Vec<u8>,
    off: u64,
}

#[derive(Serialize, Deserialize)]
struct Field {
    offset: u64,
    name: String,
    typename: String,
}

#[derive(Serialize, Deserialize)]
struct Structure {
    size: u64,
    anon: bool,
    fields: Vec<Field>,
}

type Union = Structure;

#[derive(Serialize, Deserialize)]
struct Pointer {
    size: u64,
    target: String,
}

#[derive(Serialize, Deserialize)]
struct Typedef {
    target: String,
}

#[derive(Serialize, Deserialize)]
struct Parameter {
    name: String,
    typename: String,
}

#[derive(Serialize, Deserialize)]
struct Function {
    parameters: Vec<Parameter>,
    returntype: String,
}

#[derive(Serialize, Deserialize)]
struct Array {
    count: u64,
    target: String,
}

#[derive(Serialize, Deserialize)]
struct EnumField {
    name: String,
    // can a backing enum type be larger than u64?
    value: u64,
}

#[derive(Serialize, Deserialize)]
struct Enum {
    size: u64,
    signed: bool,
    fields: Vec<EnumField>,
}

#[derive(Serialize, Deserialize)]
struct Integer {
    size: u64,
    signed: bool,
}

#[derive(Serialize, Deserialize)]
struct GlobalVariable {
    name: String,
    size: u64,
    typename: String,
}

enum BinjaType {
    Structure(Structure),
    Union(Union),
    Integer(Integer),
    Pointer(Pointer),
    Typedef(Typedef),
    Function(Function),
    Enum(Enum),
    Array(Array),
}

type DynErr = Box<dyn std::error::Error>;
type Err = Result<(), DynErr>;

fn collect_types() -> Result<HashMap<String, BinjaType>, DynErr> {
    let mut types = HashMap::new();

    let structs: HashMap<String, Structure> =
        serde_json::from_str(&fs::read_to_string("structs.json")?)?;
    structs.into_iter().for_each(|(k, v)| {
        _ = types.insert(k, BinjaType::Structure(v));
    });

    let unions: HashMap<String, Union> = serde_json::from_str(&fs::read_to_string("unions.json")?)?;
    unions.into_iter().for_each(|(k, v)| {
        _ = types.insert(k, BinjaType::Union(v));
    });

    let integers: HashMap<String, Integer> =
        serde_json::from_str(&fs::read_to_string("integers.json")?)?;
    integers.into_iter().for_each(|(k, v)| {
        _ = types.insert(k, BinjaType::Integer(v));
    });

    let pointers: HashMap<String, Pointer> =
        serde_json::from_str(&fs::read_to_string("pointers.json")?)?;
    pointers.into_iter().for_each(|(k, v)| {
        _ = types.insert(k, BinjaType::Pointer(v));
    });

    let typedefs: HashMap<String, Typedef> =
        serde_json::from_str(&fs::read_to_string("typedefs.json")?)?;
    typedefs.into_iter().for_each(|(k, v)| {
        _ = types.insert(k, BinjaType::Typedef(v));
    });

    let functions: HashMap<String, Function> =
        serde_json::from_str(&fs::read_to_string("functions.json")?)?;
    functions.into_iter().for_each(|(k, v)| {
        _ = types.insert(k, BinjaType::Function(v));
    });

    let enums: HashMap<String, Enum> = serde_json::from_str(&fs::read_to_string("enums.json")?)?;
    enums.into_iter().for_each(|(k, v)| {
        _ = types.insert(k, BinjaType::Enum(v));
    });

    let arrays: HashMap<String, Array> = serde_json::from_str(&fs::read_to_string("arrays.json")?)?;
    arrays.into_iter().for_each(|(k, v)| {
        _ = types.insert(k, BinjaType::Array(v));
    });

    Ok(types)
}

fn collect_variables() -> Result<HashMap<u64, GlobalVariable>, DynErr> {
    Ok(serde_json::from_str(&fs::read_to_string(
        "variables.json",
    )?)?)
}

fn visit(
    dwarf: &mut DwarfUnit,
    mappings: &HashMap<String, BinjaType>,
    dwarf_types: &mut HashMap<String, gimli::write::UnitEntryId>,
    name: &String,
) {
    if dwarf_types.contains_key(name) || name.len() == 0 {
        return;
    }

    let binja_type = mappings.get(name).unwrap();
    let tag = match binja_type {
        BinjaType::Structure(_) => gimli::DW_TAG_structure_type,
        BinjaType::Union(_) => gimli::DW_TAG_union_type,
        BinjaType::Integer(_) => gimli::DW_TAG_base_type,
        BinjaType::Pointer(_) => gimli::DW_TAG_pointer_type,
        BinjaType::Typedef(_) => gimli::DW_TAG_typedef,
        BinjaType::Function(_) => gimli::DW_TAG_subroutine_type,
        BinjaType::Enum(_) => gimli::DW_TAG_enumeration_type,
        BinjaType::Array(_) => gimli::DW_TAG_array_type,
    };
    dwarf_types.insert(name.clone(), dwarf.unit.add(dwarf.unit.root(), tag));

    match binja_type {
        BinjaType::Structure(s) => s.fields.iter().for_each(
            |Field {
                 typename,
                 offset: _,
                 name: _,
             }| visit(dwarf, mappings, dwarf_types, typename),
        ),
        BinjaType::Union(u) => u.fields.iter().for_each(
            |Field {
                 typename,
                 offset: _,
                 name: _,
             }| visit(dwarf, mappings, dwarf_types, typename),
        ),
        BinjaType::Pointer(p) => visit(dwarf, mappings, dwarf_types, &p.target),
        BinjaType::Typedef(t) => visit(dwarf, mappings, dwarf_types, &t.target),
        BinjaType::Function(f) => {
            visit(dwarf, mappings, dwarf_types, &f.returntype);
            f.parameters
                .iter()
                .for_each(|Parameter { name: _, typename }| {
                    visit(dwarf, mappings, dwarf_types, typename)
                });
        }
        BinjaType::Array(a) => visit(dwarf, mappings, dwarf_types, &a.target),
        _ => {}
    }
}

pub fn main() -> Err {
    unsafe {
        let name = "test.o";
        let mut file = File::create(Path::new(name))?;

        let mut ident: [u8; SIZEOF_IDENT] = [0u8; 16];
        for i in 0..4 {
            ident[i] = ELFMAG[i];
        }
        ident[EI_ABIVERSION] = 0;
        ident[EI_CLASS] = ELFCLASS64;
        ident[EI_DATA] = ELFDATA2LSB;
        ident[EI_OSABI] = ELFOSABI_SYSV;
        ident[EI_VERSION] = 1;
        let mut header = Header {
            e_ident: ident,
            e_type: ET_EXEC,
            e_machine: EM_X86_64,
            e_version: 1,
            e_entry: 0,
            e_phoff: 0,
            e_shoff: 0,
            e_flags: 0,
            e_ehsize: SIZEOF_EHDR as u16,
            e_phentsize: segment::SIZEOF_PHDR as u16,
            e_phnum: 0,
            e_shentsize: section::SIZEOF_SHDR as u16,
            e_shnum: 0,
            e_shstrndx: 0,
        };

        let mut sections: HashMap<String, Section> = HashMap::new();
        let mut symbols: HashMap<String, RawSymbol> = HashMap::new();

        sections.insert(
            String::from(".text"),
            Section {
                hdr: RawSection {
                    sh_type: section::SHT_PROGBITS,
                    sh_flags: (section::SHF_EXECINSTR | section::SHF_ALLOC) as u64,
                    ..Default::default()
                },
                raw: Vec::new(),
                off: 0,
            },
        );

        // Choose the encoding parameters.
        let encoding = gimli::Encoding {
            format: gimli::Format::Dwarf64,
            version: 4,
            address_size: 8,
        };
        // Create a container for a single compilation unit.
        let mut dwarf = DwarfUnit::new(encoding);
        // // Set a range attribute on the root DIE.
        // let range_list = RangeList(vec![Range::StartLength {
        //     begin: Address::Constant(0x10000),
        //     length: 0x1337,
        // }]);
        // let range_list_id = dwarf.unit.ranges.add(range_list);
        let root = dwarf.unit.root();
        // dwarf.unit.get_mut(root).set(
        //     gimli::DW_AT_ranges,
        //     AttributeValue::RangeListRef(range_list_id),
        // );

        let type_mapping = collect_types()?;
        let global_variables = collect_variables()?;
        let mut dwarf_types: HashMap<String, gimli::write::UnitEntryId> = HashMap::new();
        for name in type_mapping.keys() {
            visit(&mut dwarf, &type_mapping, &mut dwarf_types, name);
        }

        let base_type = |bytes: u64, signed: bool| {
            return *dwarf_types
                .get(&format!(
                    "{}int{}_t",
                    if signed { "" } else { "u" },
                    bytes * 8,
                ))
                .unwrap();
        };

        for (name, binja_type) in type_mapping.into_iter() {
            match binja_type {
                BinjaType::Structure(Structure { size, anon, fields }) => {
                    let id = *dwarf_types.get(&name).unwrap();
                    let unit = dwarf.unit.get_mut(id);
                    if !anon {
                        unit.set(
                            gimli::DW_AT_name,
                            AttributeValue::StringRef(dwarf.strings.add(name)),
                        );
                    }
                    unit.set(gimli::DW_AT_byte_size, AttributeValue::Udata(size));

                    for Field {
                        offset,
                        name,
                        typename,
                    } in fields
                    {
                        let id = dwarf.unit.add(id, gimli::DW_TAG_member);
                        let field = dwarf.unit.get_mut(id);
                        field.set(
                            gimli::DW_AT_name,
                            AttributeValue::StringRef(dwarf.strings.add(name)),
                        );
                        field.set(
                            gimli::DW_AT_type,
                            AttributeValue::UnitRef(*dwarf_types.get(&typename).unwrap()),
                        );
                        field.set(
                            gimli::DW_AT_data_member_location,
                            AttributeValue::Udata(offset),
                        );
                    }
                }
                BinjaType::Union(Union { size, anon, fields }) => {
                    let id = *dwarf_types.get(&name).unwrap();
                    let unit = dwarf.unit.get_mut(id);
                    if !anon {
                        unit.set(
                            gimli::DW_AT_name,
                            AttributeValue::StringRef(dwarf.strings.add(name)),
                        );
                    }
                    unit.set(gimli::DW_AT_byte_size, AttributeValue::Udata(size));

                    for Field {
                        offset,
                        name,
                        typename,
                    } in fields
                    {
                        let id = dwarf.unit.add(id, gimli::DW_TAG_member);
                        let field = dwarf.unit.get_mut(id);
                        field.set(
                            gimli::DW_AT_name,
                            AttributeValue::StringRef(dwarf.strings.add(name)),
                        );
                        field.set(
                            gimli::DW_AT_type,
                            AttributeValue::UnitRef(*dwarf_types.get(&typename).unwrap()),
                        );
                        field.set(
                            gimli::DW_AT_data_member_location,
                            AttributeValue::Udata(offset),
                        );
                    }
                }
                BinjaType::Integer(Integer { size, signed }) => {
                    let unit = dwarf.unit.get_mut(*dwarf_types.get(&name).unwrap());
                    unit.set(
                        gimli::DW_AT_name,
                        AttributeValue::StringRef(dwarf.strings.add(name)),
                    );
                    unit.set(gimli::DW_AT_byte_size, AttributeValue::Udata(size));
                    unit.set(
                        gimli::DW_AT_encoding,
                        AttributeValue::Encoding(if signed {
                            gimli::DW_ATE_signed
                        } else {
                            gimli::DW_ATE_unsigned
                        }),
                    );
                }
                BinjaType::Pointer(Pointer { size, target }) => {
                    let unit = dwarf.unit.get_mut(*dwarf_types.get(&name).unwrap());
                    unit.set(gimli::DW_AT_byte_size, AttributeValue::Udata(size));
                    if target.len() > 0 {
                        unit.set(
                            gimli::DW_AT_type,
                            AttributeValue::UnitRef(*dwarf_types.get(&target).unwrap()),
                        );
                    }
                }
                BinjaType::Typedef(Typedef { target }) => {
                    let unit = dwarf.unit.get_mut(*dwarf_types.get(&name).unwrap());
                    unit.set(
                        gimli::DW_AT_name,
                        AttributeValue::StringRef(dwarf.strings.add(name)),
                    );
                    unit.set(
                        gimli::DW_AT_type,
                        AttributeValue::UnitRef(*dwarf_types.get(&target).unwrap()),
                    );
                }
                BinjaType::Function(Function {
                    parameters,
                    returntype,
                }) => {
                    let id = *dwarf_types.get(&name).unwrap();
                    let unit = dwarf.unit.get_mut(id);
                    unit.set(gimli::DW_AT_prototyped, AttributeValue::Flag(true));
                    if returntype.len() > 0 {
                        unit.set(
                            gimli::DW_AT_type,
                            AttributeValue::UnitRef(*dwarf_types.get(&returntype).unwrap()),
                        );
                    }

                    for Parameter { name, typename } in parameters {
                        let id = dwarf.unit.add(id, gimli::DW_TAG_formal_parameter);
                        let unit = dwarf.unit.get_mut(id);
                        if name.len() > 0 {
                            unit.set(
                                gimli::DW_AT_name,
                                AttributeValue::StringRef(dwarf.strings.add(name)),
                            );
                        }
                        unit.set(
                            gimli::DW_AT_type,
                            AttributeValue::UnitRef(*dwarf_types.get(&typename).unwrap()),
                        );
                    }
                }
                BinjaType::Enum(Enum {
                    size,
                    signed,
                    fields,
                }) => {
                    let id = *dwarf_types.get(&name).unwrap();
                    let unit = dwarf.unit.get_mut(id);
                    unit.set(
                        gimli::DW_AT_name,
                        AttributeValue::StringRef(dwarf.strings.add(name)),
                    );
                    unit.set(gimli::DW_AT_byte_size, AttributeValue::Udata(size));
                    unit.set(
                        gimli::DW_AT_encoding,
                        AttributeValue::Encoding(if signed {
                            gimli::DW_ATE_signed
                        } else {
                            gimli::DW_ATE_unsigned
                        }),
                    );
                    unit.set(
                        gimli::DW_AT_type,
                        AttributeValue::UnitRef(base_type(size, signed)),
                    );

                    for EnumField { name, value } in fields {
                        let id = dwarf.unit.add(id, gimli::DW_TAG_enumerator);
                        let field = dwarf.unit.get_mut(id);
                        field.set(
                            gimli::DW_AT_name,
                            AttributeValue::StringRef(dwarf.strings.add(name)),
                        );
                        field.set(gimli::DW_AT_const_value, AttributeValue::Udata(value));
                    }
                }
                BinjaType::Array(Array { count, target }) => {
                    let id = *dwarf_types.get(&name).unwrap();
                    let unit = dwarf.unit.get_mut(id);

                    unit.set(
                        gimli::DW_AT_type,
                        AttributeValue::UnitRef(*dwarf_types.get(&target).unwrap()),
                    );

                    let id = dwarf.unit.add(id, gimli::DW_TAG_subrange_type);
                    let unit = dwarf.unit.get_mut(id);

                    unit.set(
                        gimli::DW_AT_type,
                        AttributeValue::UnitRef(base_type(8, false)),
                    );
                    unit.set(gimli::DW_AT_upper_bound, AttributeValue::Udata(count - 1));
                }
                _ => {}
            }
        }

        for (
            address,
            GlobalVariable {
                name,
                size,
                typename,
            },
        ) in global_variables.into_iter()
        {
            let id = dwarf.unit.add(root, gimli::DW_TAG_variable);
            let unit = dwarf.unit.get_mut(id);
            unit.set(
                gimli::DW_AT_name,
                AttributeValue::StringRef(dwarf.strings.add(name.clone())),
            );
            if typename.len() > 0 {
                unit.set(
                    gimli::DW_AT_type,
                    AttributeValue::UnitRef(*dwarf_types.get(&typename).unwrap()),
                );
            }
            unit.set(gimli::DW_AT_external, AttributeValue::Flag(true));
            let mut location = Expression::new();
            location.op_addr(Address::Constant(address));
            unit.set(gimli::DW_AT_location, AttributeValue::Exprloc(location));

            symbols.insert(
                name,
                RawSymbol {
                    st_name: 0,
                    // 0x10 <- global binding
                    // 0x01 <- object type
                    st_info: 0x11,
                    st_other: 0,
                    // TODO: parse original elf for section mappings
                    st_shndx: 0,
                    st_size: size,
                    // assumed to be non rebased offset
                    st_value: address,
                },
            );
        }

        // set CU attributes
        let comp_dir_name = String::from("llvm-dwarf");
        let comp_dir_name_id = dwarf.strings.add(comp_dir_name);
        let comp_dir = LineString::StringRef(comp_dir_name_id);
        dwarf.unit.get_mut(root).set(
            gimli::DW_AT_comp_dir,
            AttributeValue::StringRef(comp_dir_name_id),
        );

        let comp_file_name = String::from("debuginfo.c");
        let comp_file_name_id = dwarf.strings.add(comp_file_name);
        let comp_file = LineString::StringRef(comp_file_name_id);
        dwarf.unit.get_mut(root).set(
            gimli::DW_AT_name,
            AttributeValue::StringRef(comp_file_name_id),
        );

        dwarf.unit.get_mut(root).set(
            gimli::DW_AT_low_pc,
            AttributeValue::Address(Address::Constant(0)),
        );
        dwarf.unit.get_mut(root).set(
            gimli::DW_AT_high_pc,
            AttributeValue::Address(Address::Constant(0x1337)),
        );
        dwarf.unit.get_mut(root).set(
            gimli::DW_AT_language,
            AttributeValue::Language(gimli::DW_LANG_C),
        );

        let producer = String::from(":3");
        let producer_id = dwarf.strings.add(producer);
        dwarf.unit.get_mut(root).set(
            gimli::DW_AT_producer,
            AttributeValue::StringRef(producer_id),
        );

        // dwarf.unit.line_program =
        //     LineProgram::new(encoding, LineEncoding::default(), comp_dir, comp_file, None);
        // let directory_id = dwarf.unit.line_program.add_directory(LineString::String(
        //     dwarf.strings.get(comp_dir_name_id).to_vec(),
        // ));
        // let file_id = dwarf.unit.line_program.add_file(
        //     LineString::String(dwarf.strings.get(comp_file_name_id).to_vec()),
        //     directory_id,
        //     None,
        // );
        // dwarf
        //     .unit
        //     .line_program
        //     .begin_sequence(Some(Address::Constant(0)));
        // dwarf.unit.line_program.row().file = file_id;
        // dwarf.unit.line_program.row().address_offset = 0;
        // dwarf.unit.line_program.row().is_statement = true;
        // dwarf.unit.line_program.row().line = 13;
        // dwarf.unit.line_program.row().column = 69;
        // dwarf.unit.line_program.generate_row();
        // dwarf.unit.line_program.end_sequence(4);

        // Create a `Vec` for each DWARF section.
        let mut dwarf_sections = Sections::new(EndianVec::new(gimli::LittleEndian));
        dwarf.write(&mut dwarf_sections)?;

        // Finally, write the DWARF data to the sections.
        dwarf_sections.for_each(|id, data| {
            // Here you can add the data to the output object file.
            sections.insert(
                String::from(id.name()),
                Section {
                    hdr: section::SectionHeader {
                        sh_type: section::SHT_PROGBITS,
                        ..Default::default()
                    },
                    raw: data.clone().into_vec(),
                    off: 0,
                },
            );

            Err::Ok(())
        })?;

        // finalize elf file
        let mut section_names = Section {
            hdr: RawSection {
                sh_type: section::SHT_STRTAB,
                ..Default::default()
            },
            raw: Vec::new(),
            off: 0,
        };

        let mut symbol_table = Section {
            hdr: RawSection {
                sh_type: section::SHT_SYMTAB,
                sh_link: 2,
                sh_entsize: SIZEOF_SYM as u64,
                ..Default::default()
            },
            raw: Vec::new(),
            off: 0,
        };

        let mut symbol_names = Section {
            hdr: RawSection {
                sh_type: section::SHT_STRTAB,
                ..Default::default()
            },
            raw: Vec::new(),
            off: 0,
        };

        sections.insert(String::from(".symtab"), symbol_table);

        // account for NULL section
        header.e_shnum += 1;

        // account for section names table
        header.e_shnum += 1;

        // account for symbol names table
        header.e_shnum += 1;

        // account for all the dwarf sections
        header.e_shnum += sections.len() as u16;

        // set section table start
        header.e_shoff = SIZEOF_EHDR as u64;

        // set section names index
        header.e_shstrndx = 1;

        file.write(&transmute::<_, [u8; SIZEOF_EHDR]>(header))?;

        // calculate where section data starts
        let section_contents_start =
            file.stream_position()? + header.e_shnum as u64 * SIZEOF_SHDR as u64;
        let mut section_contents_offset = section_contents_start;

        file.seek(SeekFrom::Start(section_contents_offset))?;
        section_names.hdr.sh_offset = section_contents_offset;

        // emit section names

        file.write(b"\x00")?;
        // write .shstrtab name
        section_names.hdr.sh_name = (file.stream_position()? - section_names.hdr.sh_offset) as u32;
        file.write(b".shstrtab\x00")?;

        for (name, section) in sections.iter_mut() {
            section.hdr.sh_name = (file.stream_position()? - section_names.hdr.sh_offset) as u32;
            file.write(name.as_bytes())?;
            file.write(b"\x00")?;
        }
        file.write(b"\x00")?;

        section_contents_offset = file.stream_position()?;
        section_names.hdr.sh_size = section_contents_offset - section_names.hdr.sh_offset;

        // emit symbol names

        symbol_names.hdr.sh_offset = section_contents_offset;
        file.write(b"\x00")?;

        for (name, symbol) in symbols.iter_mut() {
            symbol.st_name = (file.stream_position()? - symbol_names.hdr.sh_offset) as u32;
            file.write(name.as_bytes())?;
            file.write(b"\x00")?;
        }
        file.write(b"\x00")?;

        // fill out symtab contents

        sections.get_mut(".symtab").unwrap().raw = symbols
            .values()
            .map(|sym| (&transmute::<_, [u8; SIZEOF_SYM]>(*sym)).to_vec())
            .fold(vec![0u8; SIZEOF_SYM], |a, b| [a, b].concat());

        section_contents_offset = file.stream_position()?;
        symbol_names.hdr.sh_size = section_contents_offset - symbol_names.hdr.sh_offset;

        for (_, section) in sections.iter_mut() {
            file.seek(SeekFrom::Start(section_contents_offset))?;
            file.write(section.raw.as_slice())?;

            section.hdr.sh_offset = section_contents_offset;
            section.hdr.sh_size = file.stream_position()? - section_contents_offset;

            section_contents_offset = file.stream_position()?;
        }

        // seek to section headers
        file.seek(SeekFrom::Start(header.e_shoff))?;

        // write NULL section
        file.write(&transmute::<_, [u8; SIZEOF_SHDR]>(RawSection {
            ..Default::default()
        }))?;

        // write section names
        file.write(&transmute::<_, [u8; SIZEOF_SHDR]>(section_names.hdr))?;

        // write symbol names
        file.write(&transmute::<_, [u8; SIZEOF_SHDR]>(symbol_names.hdr))?;

        // write rest of sections
        for (name, section) in sections.iter() {
            println!("section name: {}", name);
            file.write(&transmute::<_, [u8; SIZEOF_SHDR]>(section.hdr))?;
        }

        Err::Ok(())
    }
}
