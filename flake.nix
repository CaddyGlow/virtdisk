{
  description = "Rust and Windows cross-compilation";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    { nixpkgs, rust-overlay, ... }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      pkgsFor =
        system:
        import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };
    in
    {
      formatter = forAllSystems (system: (pkgsFor system).nixfmt);
      devShells = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          rust = pkgs.rust-bin.stable."1.99.0".default.override {
            extensions = [ "rust-src" ];
            targets = [
              "x86_64-pc-windows-msvc"
              "i686-pc-windows-msvc"
            ];
          };
          nightly = pkgs.rust-bin.selectLatestNightlyWith (
            toolchain: toolchain.default.override { extensions = [ "rust-src" ]; }
          );
          mkDevShell =
            toolchain: extraPackages:
            pkgs.mkShell {
              packages = [
                toolchain
                pkgs.go-task
                pkgs.python3
                pkgs.git-lfs
                pkgs.sccache
                pkgs.nixfmt
                pkgs.cargo-xwin
                pkgs.clang
                pkgs.lld
                pkgs.llvm
                pkgs.actionlint
                pkgs.powershell
                pkgs.shellcheck
                # Native dependencies for honggfuzz's instrumented builds.
                pkgs.gnumake
                pkgs.binutils-unwrapped
                pkgs.libunwind
                pkgs.xz
              ]
              ++ extraPackages;
              XWIN_ARCH = "x86,x86_64";
              shellHook = "";
            };
        in
        {
          default = mkDevShell rust [ ];
          fuzz = mkDevShell nightly [ pkgs.cargo-fuzz ];
        }
      );
    };
}
