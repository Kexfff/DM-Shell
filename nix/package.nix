{
  lib,
  rustPlatform,
  pkg-config,
  makeWrapper,
  makeFontsConf,
  libxkbcommon,
  wayland,
  fontconfig,
  dejavu_fonts,
  curl,
  pulseaudio,
}:

let
  manifest = (lib.importTOML ../Cargo.toml).workspace.package;
in
rustPlatform.buildRustPackage {
  pname = "dmsh";
  inherit (manifest) version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../crates
    ];
  };
  cargoLock.lockFile = ../Cargo.lock;

  nativeBuildInputs = [
    pkg-config
    makeWrapper
  ];
  buildInputs = [
    libxkbcommon
    wayland
  ];

  # The tests lay out real text, so they need a font and fontconfig's tools.
  nativeCheckInputs = [ fontconfig ];
  FONTCONFIG_FILE = makeFontsConf { fontDirectories = [ dejavu_fonts ]; };
  preCheck = ''
    export HOME=$TMPDIR
  '';

  # Programs dmsh runs: fontconfig to find fonts, pactl for volume events, curl for the
  # weather. Appended to PATH, so the system's own (matching the running audio server)
  # come first. wpctl isn't bundled: it comes with PipeWire on any system that has it,
  # and WirePlumber would pull in all of PipeWire.
  postInstall = ''
    wrapProgram $out/bin/dmsh --suffix PATH : ${
      lib.makeBinPath [
        fontconfig
        pulseaudio
        curl
      ]
    }
  '';

  meta = {
    description = "A minimal dynamic-island shell for niri";
    homepage = "https://github.com/Kexfff/DM-Shell";
    license = lib.licenses.gpl3Plus;
    platforms = lib.platforms.linux;
    mainProgram = "dmsh";
  };
}
