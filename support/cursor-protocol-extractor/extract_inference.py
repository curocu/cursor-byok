"""Extract the compact protobuf descriptors shipped in Cursor's Agent harness.

Reads JavaScript as text; never executes the installed application bundle.
"""
import argparse
import json
import re
from pathlib import Path


def extract(source):
    prefixes = dict(re.findall(r'var ([\w$]+)="([\w.]+\.)",\w+=class extends xe', source))
    bases = {m[3]: m[2] for m in re.finditer(
        r'var ([\w$]+)="([\w.]+\.)",([\w$]+)=class extends xe', source)}
    types = {}
    classes = list(re.finditer(r'([\w$]+)=class t extends ([\w$]+)\{', source))
    for index, match in enumerate(classes):
        end = classes[index + 1].start() if index + 1 < len(classes) else len(source)
        segment = source[match.end():end]
        descriptor = re.search(r'static \$\(\)\{return\[("[^"\n]*")([^\]]*)\]', segment)
        if not descriptor or match[2] not in bases:
            continue
        fields = json.loads(descriptor[1]).split('|')
        types[match[1]] = dict(name=bases[match[2]] + fields[0], fields=fields[1:],
                               refs=descriptor[2].lstrip(',').split(',') if descriptor[2] else [])
    for match in re.finditer(r'([\w$]+)=Re\(l,([\w$]+),"([\w.]+)",(\[\[.*?\]\])(?:,[^)]*)?\)', source):
        if match[2] in prefixes:
            types[match[1]] = dict(name=prefixes[match[2]] + match[3], enum=json.loads(match[4]))
    for key, name in re.findall(r'([\w$]+)\.typeName="(google\.protobuf\.[\w.]+)"', source):
        types[key] = dict(name=name, external=True)
    return types


def generate(types, roots):
    names = {item['name']: key for key, item in types.items()}
    selected = set()
    def visit(key):
        if key in selected:
            return
        if key not in types:
            raise ValueError('Unresolved descriptor: ' + key)
        selected.add(key)
        item = types[key]
        for field in item.get('fields', []):
            for ref in re.findall(r'#(\d+)', field):
                visit(item['refs'][int(ref)])
    for root in roots:
        visit(names[root])
    short = {}
    for key in selected:
        name = types[key]['name']
        short[key] = '.' + name if types[key].get('external') else '.'.join(name.split('.')[2:]).replace('.', '_')
    if len(set(short.values())) != len(short):
        raise ValueError('Duplicate short names')
    scalars = {1:'double',2:'float',3:'int64',4:'uint64',5:'int32',6:'fixed64',7:'fixed32',
               8:'bool',9:'string',12:'bytes',13:'uint32',15:'sfixed32',16:'sfixed64',17:'sint32',18:'sint64'}
    def field_type(token, item):
        if token.startswith('#'):
            return short[item['refs'][int(token[1:])]]
        return scalars[int(token)]
    lines = ['// Extracted from Cursor 3.27.22 Agent harness b951911e4476.',
             '// Wire contracts only; no application code is executed or embedded.',
             'syntax = "proto3";', 'package cursorbyok.inference;', '']
    for filename in ['struct', 'empty', 'timestamp', 'duration', 'wrappers', 'any']:
        lines.append(f'import "google/protobuf/{filename}.proto";')
    for key in sorted(selected, key=lambda k: types[k]['name']):
        item = types[key]
        if item.get('external'):
            continue
        lines.append('// ' + item['name'])
        if 'enum' in item:
            lines.append('enum ' + short[key] + ' {')
            if len({entry[0] for entry in item['enum']}) != len(item['enum']):
                lines.append('  option allow_alias = true;')
            for number, name in item['enum']:
                lines.append(f'  {short[key].upper()}_{name} = {number};')
            lines.extend(['}', ''])
            continue
        lines.append('message ' + short[key] + ' {')
        oneofs = {}
        for field in item['fields']:
            parts = field.split(' ')
            number, name, token = parts[:3]
            marker = parts[3] if len(parts) > 3 and not parts[3].startswith('=') else ''
            if token[-1:] in ('?', '*'):
                marker, token = token[-1], token[:-1]
            if ',' in token:
                left, right = token.split(',')
                definition = f'map<{field_type(left,item)}, {field_type(right,item)}> {name} = {number};'
            else:
                modifier = {'?':'optional ', '*':'repeated '}.get(marker, '')
                definition = f'{modifier}{field_type(token,item)} {name} = {number};'
            if marker not in ('', '?', '*'):
                oneofs.setdefault(marker, []).append(definition)
            else:
                lines.append('  ' + definition)
        for name, fields in oneofs.items():
            lines.append('  oneof ' + name + ' {')
            lines.extend('    ' + field for field in fields)
            lines.append('  }')
        lines.extend(['}', ''])
    return '\n'.join(lines)


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('bundle', type=Path)
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    types = extract(args.bundle.read_text(encoding='utf-8'))
    result = generate(types, ['aiserver.v1.RunInferenceClientMessage', 'aiserver.v1.RunInferenceServerMessage'])
    args.output.write_bytes(result.encode('utf-8'))
    print(f'Extracted {len(types)} descriptors; generated {len(result.splitlines())} schema lines')
