{
  pkgs,
  lib,
  config,
  inputs,
  ...
}:

{
  packages = with pkgs; [
    librime
    pkg-config
    cargo
    rustc
  ];

}
