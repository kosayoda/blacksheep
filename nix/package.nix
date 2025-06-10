{
  lib,
  rustPlatform,
  symlinkJoin,
  writeShellScriptBin,
  pkg-config,
  qt6,
  kdePackages,
  pipewire,
}:
let
  # cxx-qt asks qmake where Qt's headers, libraries and helper tools are, and
  # qmake only knows about qtbase. Every module blacksheep needs at build or
  # run time is joined into one tree, and a qt.conf makes qmake report that
  # tree instead of its own store path.
  qtModules = [
    qt6.qtbase
    qt6.qtdeclarative
    qt6.qtwayland
    qt6.qtsvg
    kdePackages.layer-shell-qt
  ];
  qt = symlinkJoin {
    name = "blacksheep-qt";
    paths = lib.concatMap (m: [ (lib.getOutput "out" m) (lib.getDev m) ]) qtModules;
    postBuild = ''
      cat > $out/qt.conf <<CONF
      [Paths]
      Prefix=$out
      Plugins=lib/qt-6/plugins
      QmlImports=lib/qt-6/qml
      CONF
    '';
  };
  qmake = writeShellScriptBin "qmake" ''
    exec ${qt}/bin/qmake -qtconf ${qt}/qt.conf "$@"
  '';
  cargoToml = lib.importTOML ../Cargo.toml;
in
rustPlatform.buildRustPackage (finalAttrs: {
  pname = "blacksheep";
  version = cargoToml.workspace.package.version;

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
    rustPlatform.bindgenHook
    qt6.wrapQtAppsHook
  ];
  buildInputs = [
    qt
    pipewire
  ] ++ qtModules;

  # qtbase's setup hook exports its own QMAKE, so this has to come later.
  preConfigure = ''
    export QMAKE=${qmake}/bin/qmake
  '';

  cargoBuildFlags = [ "--package" "blacksheep" ];

  passthru.qmake = "${qmake}/bin/qmake";

  meta = {
    description = "Wayland shell host";
    homepage = "https://github.com/kosayoda/blacksheep";
    license = with lib.licenses; [ mit asl20 ];
    mainProgram = "blacksheep";
    platforms = lib.platforms.linux;
  };
})
