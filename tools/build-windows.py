"""Build the Windows x64 player on Linux (a cross build with llvm-mingw).

Downloads the pinned llvm-mingw release into vendor/ (checked by SHA-256),
builds `ac3.exe` for x86_64-pc-windows-gnullvm, and writes dist/windows/
(the executable, its runtime DLLs, the CRT looks, build.json), then the zip
to share (tools/package-windows.py). Needs rustup and Python 3.9+.
"""
from pathlib import Path
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tarfile
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
NAME ='llvm-mingw-20260908-ucrt-ubuntu-22.04-x86_64'
SHA256 = '2258c745e3155870c80793f3e8c80b28fbde11b9ff73c4c78783635b3440b092'
URL = f'https://github.com/mstorsjo/llvm-mingw/releases/download/20260908/{NAME}.tar.xz'
TARGET = 'x86_64-pc-windows-gnullvm'


def environment():
    """The environment, with a Rust toolchain kept in vendor/rust if there is one."""
    env = os.environ.copy()
    cargo = ROOT / 'vendor/rust/cargo'
    if (cargo / 'bin/cargo').is_file():
        env.update(CARGO_HOME=str(cargo), RUSTUP_HOME=str(ROOT / 'vendor/rust/rustup'))
        env['PATH'] = str(cargo / 'bin') + os.pathsep + env.get('PATH', '')
    return env


def main():
    if os.name != 'posix':
        raise SystemExit('Run this script on Linux; native Windows builds can use Cargo directly.')
    archive = ROOT / 'vendor' / f'{NAME}.tar.xz'
    compiler = ROOT / 'vendor' / NAME
    if not archive.exists():
        archive.parent.mkdir(parents=True, exist_ok=True)
        urllib.request.urlretrieve(URL, archive)
    if hashlib.sha256(archive.read_bytes()).hexdigest() != SHA256:
        raise SystemExit('Compiler archive SHA-256 mismatch')
    if not compiler.exists():
        with tarfile.open(archive) as source:
            source.extractall(ROOT / 'vendor', filter='data')
    env = environment()
    env['PATH'] = str(compiler / 'bin') + os.pathsep + env['PATH']
    linker = str(compiler / 'bin/x86_64-w64-mingw32-clang')
    env['CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_LINKER'] = linker
    env['CC_x86_64_pc_windows_gnullvm'] = linker
    # librashader's glslang and SPIRV-Cross are C++: llvm-mingw's clang++
    # with its libc++ (libc++.dll ships next to the executable).
    env['CXX_x86_64_pc_windows_gnullvm'] = str(compiler / 'bin/x86_64-w64-mingw32-clang++')
    env['CXXSTDLIB_x86_64_pc_windows_gnullvm'] = 'c++'
    subprocess.run(['rustup', 'target', 'add', TARGET], cwd=ROOT, env=env, check=True)
    subprocess.run(['cargo', 'build', '--release', '--target', TARGET, '--locked'],
                   cwd=ROOT, env=env, check=True)
    output = ROOT / 'dist/windows'
    output.mkdir(parents=True, exist_ok=True)
    executable = output / 'ac3.exe'
    shutil.copy2(ROOT / 'target' / TARGET / 'release/ac3.exe', executable)
    # Runtime libraries are supplied by the same pinned compiler distribution.
    # Copy only available runtime DLLs; OS DLLs continue to come from Windows.
    libraries = []
    for name in ['libunwind.dll', 'libwinpthread-1.dll', 'libc++.dll']:
        source = compiler / 'x86_64-w64-mingw32/bin' / name
        if source.is_file():
            shutil.copy2(source, output / name)
            libraries.append(name)
    # The bundled CRT looks (src/present/crt.rs), beside the executable.
    shaders = output / 'shaders/lola-retro'
    if shaders.exists():
        shutil.rmtree(shaders)
    shutil.copytree(ROOT / 'shaders/lola-retro', shaders)
    report = {'target': TARGET, 'compiler_archive_sha256': SHA256,
              'executable_sha256': hashlib.sha256(executable.read_bytes()).hexdigest(),
              'runtime_libraries': libraries,
              'game_data_bundled': False, 'runtime_tested': False}
    (output / 'build.json').write_text(json.dumps(report, indent=2) + '\n')
    print(executable)
    # The zip to share (dist/Ace-Combat-3-Windows.zip).
    subprocess.run([sys.executable, str(ROOT / 'tools/package-windows.py'), '--build', str(output)],
                   check=True)


if __name__ == '__main__':
    main()
