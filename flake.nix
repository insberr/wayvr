{
  description = "Package and development shell for wayvr";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      lib = pkgs.lib;

      buildInputs = with pkgs; [
        alsa-lib
        dav1d
        dbus
        libinput
        libx11
        libxext
        libxrandr
        libxcb
        libxkbcommon
        openssl
        openxr-loader
        pipewire
        libxcursor
        libxi
        wayland
        vulkan-loader
        openvr

        # for whisper
        vulkan-headers
        vulkan-loader
        spirv-headers
      ];
      nativeBuildInputs = with pkgs; [
        pkg-config
        rustPlatform.bindgenHook

        # for whisper
        cmake
        shaderc
        autoPatchelfHook
      ];
      features = "openxr,osc,x11,wayland,openvr,whisper";
    in
    {
      # for whisper
      dontUseCmakeConfigure = true;

      packages.${system}.default = pkgs.rustPlatform.buildRustPackage {
        pname = "wayvr";
        version = "26.8.0-local";
        src = lib.cleanSource ./.;
        cargoLock = {
          lockFile = ./Cargo.lock;
          allowBuiltinFetchGit = true;
        };
        nativeBuildInputs = nativeBuildInputs ++ [ pkgs.autoPatchelfHook ];
        inherit buildInputs;
        env.SHADERC_LIB_DIR = "${lib.getLib pkgs.shaderc}/lib";
        buildNoDefaultFeatures = true;
        buildFeatures = lib.splitString "," features;
        cargoBuildFlags = [
          "-p"
          "wayvr"
        ];
        doCheck = false;
        postInstall = ''
          install -D wayvr/wayvr.desktop -t $out/share/applications
          install -D wayvr/wayvr.svg -t $out/share/icons/hicolor/scalable/apps
        '';
        meta.mainProgram = "wayvr";
      };

      devShells.${system}.default = pkgs.mkShell {
        inherit buildInputs;
        nativeBuildInputs =
          nativeBuildInputs
          ++ (with pkgs; [
            cargo
            rustc
            rustfmt
            clippy
            rust-analyzer
          ]);
        SHADERC_LIB_DIR = "${lib.getLib pkgs.shaderc}/lib";
        LD_LIBRARY_PATH = lib.makeLibraryPath buildInputs;
      };
    };
}
