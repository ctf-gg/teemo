from collections import ChainMap
import binaryninja as binaryninja
import rpyc
import json
c = rpyc.connect("0.0.0.0", 18812)

bv: binaryninja.BinaryView = c.root.bv
bn: binaryninja = c.root.binaryninja

anonymous = 0
structs = {}
unions = {}
enums = {}
integers = {}
typedefs = {}
pointers = {}
functions = {}
arrays = {}
variables = {}

def escape(name: binaryninja.QualifiedName):
    return name.name[0]

def extract_typename(kind: binaryninja.Type, name: str | None = None):
    match kind.type_class.value:
        case binaryninja.TypeClass.IntegerTypeClass.value:
            return name or kind.get_string()
        case binaryninja.TypeClass.StructureTypeClass.value:
            if kind.registered_name is None:
                name = None
            else:
                name = escape(kind.registered_name.name)
            match kind.type.value:
                case binaryninja.StructureVariant.StructStructureType:
                    return name
                case binaryninja.StructureVariant.UnionStructureType:
                    return name
                case _:
                    exit(f"unknown structure type")
        case binaryninja.TypeClass.PointerTypeClass.value:
            return name or kind.get_string()
        case binaryninja.TypeClass.NamedTypeReferenceClass.value:
            return escape(kind.name)
        case binaryninja.TypeClass.EnumerationTypeClass.value:
            return escape(kind.registered_name.name)
        case binaryninja.TypeClass.FunctionTypeClass.value:
            return kind.get_string()
        case binaryninja.TypeClass.ArrayTypeClass.value:
            return f"{extract_typename(kind.element_type)}[{kind.count}]"
        case binaryninja.TypeClass.VoidTypeClass.value:
            return ""
        case _:
            print(f"unhandled: {name} {type(kind)}")
            exit(1)

def visit(kind: binaryninja.Type, name: str | None = None):
    global anonymous

    anon = False
    key = extract_typename(kind, name)
    if key is None:
        key = f"anon.{anonymous}"
        anonymous += 1
        anon = True

    if  key in structs or \
        key in enums or \
        key in integers or \
        key in typedefs or \
        key in pointers or \
        key in functions or \
        key in arrays:
        return key
    
    match kind.type_class.value:
        case binaryninja.TypeClass.IntegerTypeClass.value:
            integers[key] = {}
            integers[key]["size"] = len(kind)
            integers[key]["signed"] = kind.signed.value
        case binaryninja.TypeClass.StructureTypeClass.value:
            match kind.type.value:
                case binaryninja.StructureVariant.StructStructureType:
                    target = structs
                case binaryninja.StructureVariant.UnionStructureType:
                    target = unions
                case _:
                    exit(f"unknown structure type")
            target[key] = {}
            target[key]["size"] = len(kind)
            target[key]["anon"] = anon
            target[key]["fields"] = list(map(lambda field: (field.offset, field.name, visit(field.type)), kind.members))
        case binaryninja.TypeClass.PointerTypeClass.value:
            pointers[key] = {}
            pointers[key]["size"] = len(kind)
            pointers[key]["target"] = visit(kind.target)
        case binaryninja.TypeClass.NamedTypeReferenceClass.value:
            target = visit(kind.target(bv))
            if key == target:
                return key
            typedefs[key] = {}
            typedefs[key]["target"] = target
        case binaryninja.TypeClass.EnumerationTypeClass.value:
            enums[key] = {}
            enums[key]["size"] = len(kind)
            enums[key]["signed"] = kind.signed.value
            enums[key]["fields"] = list(map(lambda field: (field.name, field.value), kind.members))
        case binaryninja.TypeClass.FunctionTypeClass.value:
            functions[key] = {}
            functions[key]["parameters"] = list(map(lambda p: (p.name, visit(p.type)), kind.parameters))
            functions[key]["returntype"] = visit(kind.return_value)
        case binaryninja.TypeClass.ArrayTypeClass.value:
            arrays[key] = {}
            arrays[key]["count"] = kind.count
            arrays[key]["target"] = visit(kind.element_type)
        case binaryninja.TypeClass.VoidTypeClass.value:
            pass
        case _:
            exit(kind, name, type(kind))

    return key

for name, kind in bv.types:
    visit(kind, escape(name))

for name, symbols in bv.symbols.items():
    for symbol in symbols:
        if symbol.type.value != binaryninja.SymbolType.DataSymbol.value:
            continue

        variable = bv.get_data_var_at(symbol.address)
        variables[symbol.address] = {}
        variables[symbol.address]["name"] = name
        variables[symbol.address]["size"] = len(variable)
        variables[symbol.address]["typename"] = visit(variable.type)

json.dump(structs, open("structs.json", "w+"))
json.dump(unions, open("unions.json", "w+"))
json.dump(enums, open("enums.json", "w+"))
json.dump(integers, open("integers.json", "w+"))
json.dump(typedefs, open("typedefs.json", "w+"))
json.dump(pointers, open("pointers.json", "w+"))
json.dump(functions, open("functions.json", "w+"))
json.dump(arrays, open("arrays.json", "w+"))
json.dump(variables, open("variables.json", "w+"))