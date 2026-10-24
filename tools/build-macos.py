"""Build the macOS player as an app bundle, on a Mac.

Builds `ac3` for Apple silicon and Intel (one universal executable; --arch
picks one), and makes `dist/macos/Ace Combat 3/`:

- `Ace Combat 3.app`: the executable in Contents/MacOS, the bundled CRT
  looks and the icon (cut from the launcher's art) in Contents/Resources.
  The player's own files (settings, memory cards, quick saves, discs) live
  in ~/Library/Application Support/Ace Combat 3 (src/settings.rs,
  `player_folder`).
- `Read Me.txt`: opening an app that is not notarised, where the discs go.
- `Third-party notices.txt`: docs/THIRD-PARTY-NOTICES.txt.

The bundle is signed ad hoc (no Apple Developer ID, not notarised), then the
folder is zipped to `dist/macos/Ace-Combat-3-macOS.zip`. A report goes to
`dist/macos/build.json`. Needs the Xcode command line tools and rustup; runs
with macOS's own Python 3.9.
"""
import argparse
import hashlib
import json
import os
import plistlib
import re
import shutil
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
APP_NAME = 'Ace Combat 3'
IDENTIFIER = 'io.github.acidmiku.electrosphere'
MINIMUM_MACOS = '11.0'
TARGETS = {'arm64': 'aarch64-apple-darwin', 'x86_64': 'x86_64-apple-darwin'}

READ_ME = """Ace Combat 3: Electrosphere, native port, for macOS
==================================================

An unofficial fan port. It plays the Japanese release (two discs, Rev 1,
with the English translation patch) from your own disc images.

1. Install
   Drag "Ace Combat 3.app" into Applications (or anywhere you like).

2. The first time you open it
   The app is not notarised by Apple, so macOS refuses it at first:
   - Double-click the app. macOS says it cannot check it: click Done.
   - Open System Settings > Privacy & Security, scroll down to
     "Ace Combat 3 was blocked..." and click Open Anyway, then confirm.
   After that it opens normally.
   (Or, in Terminal:
     xattr -dr com.apple.quarantine "/Applications/Ace Combat 3.app")

3. The discs
   The launcher looks for the disc images in its folder:
     ~/Library/Application Support/Ace Combat 3
   Its "Open the folder" button opens that folder in Finder. Put the two
   discs there (.cue with its .bin, or .bin) and click "Look again".
   Expected: Ace Combat 3: Electrosphere, the Japanese release (Rev 1, two
   discs, SLPS-02020 and SLPS-02021) with the English translation patch.

4. Playing
   - The settings, memory card and quick saves are kept in the same folder.
   - On a Mac keyboard, hold fn with the F keys (F2 CRT screen, F5 / F8
     quick save / load, ...). The launcher's Play page lists them.
   - Xbox and PlayStation controllers work over USB or Bluetooth.
     Vibration does not work on macOS.
"""


def run(args, **kwargs):
    print('+', ' '.join(str(arg) for arg in args), flush=True)
    subprocess.run([str(arg) for arg in args], check=True, **kwargs)


def version():
    text = (ROOT / 'Cargo.toml').read_text(encoding='utf-8')
    return re.search(r'^version = "([^"]+)"', text, re.MULTILINE).group(1)


def build(arches):
    env = dict(os.environ, MACOSX_DEPLOYMENT_TARGET=MINIMUM_MACOS)
    run(['rustup', 'target', 'add'] + [TARGETS[arch] for arch in arches], env=env)
    outputs = []
    for arch in arches:
        run(['cargo', 'build', '--release', '--locked', '--target', TARGETS[arch]],
            cwd=ROOT, env=env)
        outputs.append(ROOT / 'target' / TARGETS[arch] / 'release/ac3')
    return outputs


def icon(resources):
    """AppIcon.icns: a square of the launcher's art (her face, on the
    right), at the iconset's sizes."""
    with tempfile.TemporaryDirectory() as scratch:
        scratch = Path(scratch)
        art = scratch / 'art.png'
        run(['sips', '-s', 'format', 'png', ROOT / 'src/launcher/background.webp',
             '--out', art], stdout=subprocess.DEVNULL)
        # The art is 1536x1024: the right-hand square holds the face.
        run(['sips', '--cropToHeightWidth', '1024', '1024', '--cropOffset', '0', '512',
             art], stdout=subprocess.DEVNULL)
        iconset = scratch / 'AppIcon.iconset'
        iconset.mkdir()
        for size in (16, 32, 128, 256, 512):
            for scale in (1, 2):
                pixels = size * scale
                name = f'icon_{size}x{size}' + ('@2x' if scale == 2 else '') + '.png'
                run(['sips', '-z', pixels, pixels, art, '--out', iconset / name],
                    stdout=subprocess.DEVNULL)
        run(['iconutil', '-c', 'icns', iconset, '-o', resources / 'AppIcon.icns'])


def bundle(executables, folder):
    app = folder / f'{APP_NAME}.app'
    contents = app / 'Contents'
    (contents / 'MacOS').mkdir(parents=True)
    resources = contents / 'Resources'
    resources.mkdir()
    executable = contents / 'MacOS/ac3'
    if len(executables) == 1:
        shutil.copy2(executables[0], executable)
    else:
        run(['lipo', '-create', '-output', executable] + executables)
    shutil.copytree(ROOT / 'shaders/lola-retro', resources / 'shaders/lola-retro')
    icon(resources)
    plist = {
        'CFBundleName': APP_NAME,
        'CFBundleDisplayName': APP_NAME,
        'CFBundleIdentifier': IDENTIFIER,
        'CFBundleExecutable': 'ac3',
        'CFBundleIconFile': 'AppIcon',
        'CFBundlePackageType': 'APPL',
        'CFBundleShortVersionString': version(),
        'CFBundleVersion': version(),
        'CFBundleInfoDictionaryVersion': '6.0',
        'LSMinimumSystemVersion': MINIMUM_MACOS,
        'LSApplicationCategoryType': 'public.app-category.action-games',
        'NSHighResolutionCapable': True,
        'NSSupportsAutomaticGraphicsSwitching': True,
    }
    with open(contents / 'Info.plist', 'wb') as file:
        plistlib.dump(plist, file)
    # Ad hoc: no identity, so not notarised; it seals the bundle's files.
    run(['codesign', '--force', '--sign', '-', '--timestamp=none', app])
    run(['codesign', '--verify', '--strict', app])
    return app, executable


def main():
    parser = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    parser.add_argument('--arch', choices=['universal'] + list(TARGETS), default='universal')
    options = parser.parse_args()
    if os.uname().sysname != 'Darwin':
        raise SystemExit('Run this script on macOS.')
    arches = list(TARGETS) if options.arch == 'universal' else [options.arch]
    executables = build(arches)
    output = ROOT / 'dist/macos'
    folder = output / APP_NAME
    if folder.exists():
        shutil.rmtree(folder)
    folder.mkdir(parents=True)
    app, executable = bundle(executables, folder)
    (folder / 'Read Me.txt').write_text(READ_ME, encoding='utf-8')
    # From tools/third-party-notices.py (librashader's MPL-2.0 source, ...).
    shutil.copy2(ROOT / 'docs/THIRD-PARTY-NOTICES.txt', folder / 'Third-party notices.txt')
    archive = output / 'Ace-Combat-3-macOS.zip'
    archive.unlink(missing_ok=True)
    run(['ditto', '-c', '-k', '--sequesterRsrc', '--keepParent', folder, archive])
    lipo = subprocess.run(['lipo', '-archs', str(executable)], check=True,
                          capture_output=True, text=True).stdout.split()
    report = {'version': version(), 'architectures': lipo,
              'minimum_macos': MINIMUM_MACOS, 'signed': 'ad hoc', 'notarised': False,
              'executable_sha256': hashlib.sha256(executable.read_bytes()).hexdigest(),
              'archive': archive.name, 'archive_bytes': archive.stat().st_size,
              'archive_sha256': hashlib.sha256(archive.read_bytes()).hexdigest(),
              'game_data_bundled': False}
    (output / 'build.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report, indent=2))
    print(app)


if __name__ == '__main__':
    main()
