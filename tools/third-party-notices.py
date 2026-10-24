"""Write docs/THIRD-PARTY-NOTICES.txt: the open-source code in the shipped
players, from `cargo tree` for the Windows and macOS targets (normal
dependencies only), with each crate's licence.

Run where Cargo can resolve both targets' dependencies (Linux or macOS),
after a dependency change; the packaging scripts copy the file into the
Windows and macOS downloads. Runs with Python 3.9.
"""
import collections
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TARGETS = ['x86_64-pc-windows-gnullvm', 'aarch64-apple-darwin', 'x86_64-apple-darwin']
OUTPUT = ROOT / 'docs/THIRD-PARTY-NOTICES.txt'

HEADER = """Ace Combat 3: Electrosphere, native port: third-party software
==============================================================

The game's own code and data are not included: the port plays your own
discs. The players are built from these open-source parts.

- librashader (the CRT screen) and the other crates below under MPL-2.0
  (librashader-*, persy, smartstring) are used unmodified. Their source
  code is at https://crates.io/crates/NAME/VERSION (for example
  https://crates.io/crates/librashader-runtime-wgpu/0.12.1); librashader's
  is also at https://github.com/SnowflakePowered/librashader. The licence:
  https://www.mozilla.org/en-US/MPL/2.0/ (crates offered under "MPL-2.0 OR
  GPL-3.0-only" are used under MPL-2.0).
- librashader builds glslang and SPIRV-Cross (C++), each under the licences
  in its own source (glslang: BSD-3-Clause and others; SPIRV-Cross:
  Apache-2.0).
- The CRT looks in shaders/lola-retro are Lola Retro by zoran123456, MIT
  (shaders/lola-retro/LICENSE).
- The launcher's fonts are egui's defaults (epaint_default_fonts): Ubuntu
  Light (Ubuntu Font Licence 1.0), Noto Emoji and emoji-icon-font (SIL Open
  Font License 1.1).
- Windows: libc++.dll and libunwind.dll are LLVM's (Apache-2.0 WITH
  LLVM-exception) and libwinpthread-1.dll is mingw-w64's (MIT-style), from
  llvm-mingw (https://github.com/mstorsjo/llvm-mingw).

The Rust crates in the players, with their licences (SPDX expressions;
where a crate offers a choice, a non-GPL one applies):
"""


def crates():
    found = {}
    for target in TARGETS:
        text = subprocess.run(
            ['cargo', 'tree', '-e', 'normal', '--target', target, '--prefix', 'none',
             '--format', '{p}|{l}', '--locked'],
            cwd=ROOT, check=True, capture_output=True, text=True).stdout
        for line in text.splitlines():
            line = line.replace(' (*)', '').replace(' (proc-macro)', '')
            if '|' not in line:
                continue
            package, licence = line.split('|', 1)
            words = package.replace(' (*)', '').replace(' (proc-macro)', '').split()
            if len(words) < 2 or (len(words) >= 3 and words[2].startswith('(')):
                continue  # the port's own crates print their folder
            name, version = words[0], words[1].lstrip('v')
            found[(name, version)] = licence.strip() or 'unknown'
    return found


def main():
    found = crates()
    by_licence = collections.Counter(found.values())
    lines = [HEADER]
    for (name, version), licence in sorted(found.items()):
        lines.append(f'  {name} {version}: {licence}')
    lines.append('')
    lines.append('Licences: ' + '; '.join(f'{licence} ({count})'
                                         for licence, count in by_licence.most_common()))
    OUTPUT.write_text('\n'.join(lines) + '\n', encoding='utf-8')
    print(f'{OUTPUT}: {len(found)} crates')
    for licence, count in by_licence.most_common():
        print(f'{count:4} {licence}')


if __name__ == '__main__':
    main()
