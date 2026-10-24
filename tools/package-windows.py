"""Zip the Windows player for sharing: `dist/Ace-Combat-3-Windows.zip`.

The zip holds one folder, `Ace Combat 3/`, with what `tools/build-windows.py`
built (ac3.exe and its runtime DLLs, checked against its build.json), the
bundled CRT looks (shaders/lola-retro, from the source), `Read Me.txt` for
the players and `Third-party notices.txt` (docs/THIRD-PARTY-NOTICES.txt,
from tools/third-party-notices.py). Nothing else from the build folder goes
in: no settings, memory cards, saves or game data. Runs anywhere (Python
3.9+); build-windows.py calls it last.
"""
import argparse
import hashlib
import json
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FOLDER = 'Ace Combat 3'

READ_ME = """Ace Combat 3: Electrosphere, native port, for Windows
===================================================

An unofficial fan port. It plays the Japanese release (two discs, Rev 1,
with the English translation patch) from your own disc images.

1. Install
   Unzip the "Ace Combat 3" folder somewhere you can write to, such as
   Documents or a Games folder (not Program Files): the settings, memory
   card and quick saves are kept in it.

2. The first time you open it
   Double-click ac3.exe. The program is not signed, so Windows may say
   "Windows protected your PC": click More info, then Run anyway.
   Your antivirus may also want to check it the first time.

3. The discs
   Put the two disc images in the same folder as ac3.exe (.cue with its
   .bin, or .bin; any names, the launcher tells them by their contents),
   then click "Look again" on the launcher's Play page ("Open the folder"
   opens the folder for you).
   Expected: Ace Combat 3: Electrosphere, the Japanese release (Rev 1, two
   discs, SLPS-02020 and SLPS-02021) with the English translation patch.

4. Playing
   - The launcher: Play, Display (window, picture, CRT screen with a
     preview) and Controls (gamepad, keyboard rebinding). Enter plays,
     Esc quits.
   - Keyboard: the arrows pitch and roll, W / S throttle, A / D yaw;
     Space or J is Square, K Cross, L Circle, I Triangle, Enter START,
     Backspace SELECT (the Controls page rebinds them).
   - Gamepads: Xbox controllers and other XInput pads. A PlayStation
     controller needs Steam Input or DS4Windows to look like one.
   - While playing: F2 CRT screen off / on, Shift+F3 the next CRT look,
     F4 curved / flat glass, F3 frame rate, F5 / F8 quick save / load,
     F6 flight effects, F7 change discs, Alt+Enter window / full screen,
     Esc quit. The launcher's Play page lists them.
   - Slow? On the Display page set the internal resolution to 2x, or
     switch the CRT screen off (F2).

5. Needs
   Windows 10 or 11 (64-bit) and a graphics card with DirectX 12 or
   Vulkan.
"""


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    parser.add_argument('--build', type=Path, default=ROOT / 'dist/windows',
                        help='the folder build-windows.py wrote (default dist/windows)')
    parser.add_argument('--output', type=Path, default=ROOT / 'dist/Ace-Combat-3-Windows.zip')
    options = parser.parse_args()
    report = json.loads((options.build / 'build.json').read_text(encoding='utf-8'))
    executable = options.build / 'ac3.exe'
    if sha256(executable) != report['executable_sha256']:
        raise SystemExit(f'{executable} is not the build build.json describes')
    notices = ROOT / 'docs/THIRD-PARTY-NOTICES.txt'
    if not notices.is_file():
        raise SystemExit(f'{notices} is missing: run tools/third-party-notices.py')
    members = [(executable, 'ac3.exe')]
    members += [(options.build / name, name) for name in report['runtime_libraries']]
    shaders = ROOT / 'shaders/lola-retro'
    members += [(path, path.relative_to(ROOT).as_posix())
                for path in sorted(shaders.rglob('*')) if path.is_file()]
    crlf = lambda text: text.replace('\r\n', '\n').replace('\n', '\r\n').encode('utf-8')
    options.output.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(options.output, 'w', zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
        for source, name in members:
            archive.write(source, f'{FOLDER}/{name}')
        archive.writestr(f'{FOLDER}/Read Me.txt', crlf(READ_ME))
        archive.writestr(f'{FOLDER}/Third-party notices.txt',
                         crlf(notices.read_text(encoding='utf-8')))
    print(json.dumps({'zip': str(options.output), 'bytes': options.output.stat().st_size,
                      'sha256': sha256(options.output), 'files': len(members) + 2,
                      'executable_sha256': report['executable_sha256']}, indent=2))


if __name__ == '__main__':
    main()
