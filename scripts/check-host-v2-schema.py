#!/usr/bin/env python3
"""Check the public message IDs and their metadata schema references."""

import json
from pathlib import Path
import re


abi = Path(__file__).resolve().parents[1] / 'doc' / 'abi'
header = (abi / 'art3m1s_core_v2.draft.h').read_text(encoding='utf-8')
values = {symbol: int(value) for symbol, value in re.findall(
    r'^#define (ART_\w+) UINT(?:16|32)_C\((\d+)\)', header, re.M,
)}


def require(condition, message):
    if not condition:
        raise SystemExit(message)


def load(file):
    return json.loads(file.read_text(encoding='utf-8'))


index = load(abi / 'artemis-messages-v2.json')
for field, macro in (
    ('abi_major', 'ART_ABI_V2_MAJOR'),
    ('submit_schema_version', 'ART_SUBMIT_SCHEMA_VERSION'),
    ('event_envelope_version', 'ART_EVENT_ENVELOPE_VERSION'),
):
    require(index[field] == values[macro], f'{field} differs from {macro}')

domains = {symbol for symbol in values if symbol.startswith('ART_DOMAIN_')}
require(set(index['domains']) == domains, 'Message index and C header domains differ')
kind_macros = header[header.index('#define ART_RUNTIME_SESSION_CHANGED'):header.index('/* Draft ARFP')]
public_kinds = set(re.findall(r'^#define (ART_\w+) ', kind_macros, re.M))
codes = index['codes']
require(codes.keys() == domains | public_kinds, 'Message codes and C header symbols differ')
for symbol, code in codes.items():
    require(type(code) is int and code == values.get(symbol), f'{symbol} differs from the published message code')
require(len({codes[domain] for domain in domains}) == len(domains), 'Duplicate message domain code')

dispatch = set()
indexed_kinds = set()
definitions = {file.name: load(file)['$defs'] for file in abi.glob('artemis-*-v2.schema.json')}


def check_refs(node, file):
    if isinstance(node, dict):
        if '$ref' in node:
            target, _, key = node['$ref'].partition('#/$defs/')
            require(key in definitions.get(target or file, {}), f"{file}: unresolved {node['$ref']}")
        for value in node.values():
            check_refs(value, file)
    elif isinstance(node, list):
        for value in node:
            check_refs(value, file)


for file, schema in definitions.items():
    check_refs(schema, file)
for domain, directions in index['domains'].items():
    name = domain.removeprefix('ART_DOMAIN_')
    prefix = 'ART_DIAGNOSTIC_' if name == 'DIAGNOSTICS' else f'ART_{name}_'
    domain_kinds = {}
    for direction, entries in directions.items():
        require(direction in ('events', 'submissions'), f'Unknown message direction: {direction}')
        for symbol, reference in entries.items():
            require(symbol in public_kinds and symbol.startswith(prefix), f'{symbol} does not belong to {domain}')
            kind = codes[symbol]
            other = domain_kinds.setdefault(kind, symbol)
            require(other == symbol, f'{domain} has duplicate kind {kind}: {other}, {symbol}')
            file, _, definition = reference.partition('#/$defs/')
            require(definition in definitions.get(file, {}), f'{symbol} points to missing {reference}')
            dispatch.add((codes[domain], direction, kind))
            indexed_kinds.add(symbol)

require(indexed_kinds == public_kinds, f'Message index and C header kinds differ: {sorted(indexed_kinds ^ public_kinds)}')
print(f'Artemis message index covers {len(indexed_kinds)} kinds in {len(dispatch)} direction-specific entries')
