# The ac3 player, built from source. The executable keeps the player's files
# in $XDG_DATA_HOME/ace-combat-3 (it lives in the read-only store) and reads
# the bundled CRT looks from share/ace-combat-3 (src/settings.rs).
{
  lib,
  rustPlatform,
  pkg-config,
  makeWrapper,
  alsa-lib,
  udev,
  vulkan-loader,
  libGL,
  libxkbcommon,
  wayland,
  xorg,
}:
let
  cargo = lib.importTOML ../Cargo.toml;
  # Loaded at run time (dlopen) by wgpu and winit: the Vulkan loader, EGL and
  # the X11 / Wayland client libraries.
  runtimeLibraries = [
    vulkan-loader
    libGL
    libxkbcommon
    wayland
    xorg.libX11
    xorg.libXcursor
    xorg.libXi
    xorg.libXrandr
  ];
in
rustPlatform.buildRustPackage {
  pname = "electrosphere";
  version = cargo.package.version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../src
      ../crates
      ../tests
      ../examples
      ../shaders
    ];
  };
  cargoLock.lockFile = ../Cargo.lock;

  nativeBuildInputs = [
    pkg-config
    makeWrapper
  ];
  buildInputs = [
    alsa-lib
    udev
  ];

  cargoBuildFlags = [
    "--bin"
    "ac3"
  ];
  # The tests need a graphics card and take minutes; run them with cargo.
  doCheck = false;

  postInstall = ''
    mkdir -p $out/share/ace-combat-3
    cp -r shaders $out/share/ace-combat-3/
    wrapProgram $out/bin/ac3 \
      --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath runtimeLibraries}
  '';

  passthru = { inherit runtimeLibraries; };

  meta = {
    description = "Ace Combat 3: Electrosphere, a native port (needs your own discs)";
    homepage = "https://github.com/acidmiku/electrosphere";
    license = lib.licenses.unfree;
    mainProgram = "ac3";
    platforms = lib.platforms.linux;
  };
}
