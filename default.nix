{
  pkgs ? import <nixpkgs> { },
}:

pkgs.rustPlatform.buildRustPackage {
  name = "rime.nvim";

  src = pkgs.lib.cleanSource ./.;

  nativeBuildInputs = [ pkgs.pkg-config ];
  buildInputs = [ pkgs.librime ];

  # 告诉 rime-sys/build.rs 去 store 里找 librime
  preBuild = ''
    export RIME_LIB_DIR=${pkgs.librime}/lib
    export RIME_INCLUDE_DIR=${pkgs.librime}/include
  '';

  cargoLock.lockFile = ./Cargo.lock;

  installPhase = ''
    runHook preInstall
    mkdir -p $out/lua $out/bin
    cp -r lua/* $out/lua/
    cp target/release/rime-daemon $out/bin/rime-daemon
    cp target/release/rime-cli $out/bin/rime-cli
    runHook postInstall
  '';
}
