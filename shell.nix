{
  pkgs ? import <nixpkgs> { },
}:
pkgs.mkShell {
  nativeBuildInputs = [ pkgs.pkg-config ];

  buildInputs = [
    pkgs.cargo
    pkgs.rustc
    pkgs.rustPlatform.bindgenHook
    pkgs.cairo
    pkgs.rust-analyzer
  ];
}
