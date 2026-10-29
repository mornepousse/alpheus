{
  description = "alpheus — a lightweight Wayland desktop shell: TUI control center, resident bar daemon and launcher bridge";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      # Only the 4 DejaVu Sans Mono files (`pkgs.dejavu_fonts` bundles the whole
      # DejaVu family — Sans, Serif, Condensed… — which fontdb would pointlessly
      # have to inventory at every start): a light fallback font, loaded only
      # for the glyphs missing from JetBrainsMono Nerd Font Mono (✳, ◐◓◑◒… in
      # window titles).
      dejaVuSansMono = pkgs: pkgs.runCommand "alpheus-dejavu-sans-mono" { } ''
        mkdir -p $out
        for f in DejaVuSansMono DejaVuSansMono-Bold DejaVuSansMono-Oblique DejaVuSansMono-BoldOblique; do
          ln -s ${pkgs.dejavu_fonts}/share/fonts/truetype/$f.ttf $out/$f.ttf
        done
      '';
      # The daemon's default font and the fallback above, joined in a single
      # folder: `Fonts::from_dir` reads only one. The folder is fixed at compile
      # time (option_env!) and read by the tests. No wrapProgram: it would
      # rename the process (.alpheus-demon-wrapped) and `pkill -x alpheus-demon`
      # would no longer find it (the same trap as waybar).
      fontDir = pkgs: "${pkgs.symlinkJoin {
        name = "alpheus-fonts";
        paths = [
          "${pkgs.nerd-fonts.jetbrains-mono}/share/fonts/truetype/NerdFonts/JetBrainsMono"
          (dejaVuSansMono pkgs)
        ];
      }}";
      # nativeBuildInputs / buildInputs: system libraries a package links
      # (the daemon's libxkbcommon, launcher plan task 12); empty by default.
      paquet = pkgs: { pname, tests, description, env ? { }, nativeBuildInputs ? [ ], buildInputs ? [ ] }:
        pkgs.rustPlatform.buildRustPackage {
          inherit pname env nativeBuildInputs buildInputs;
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          # Workspace: one package per binary. Session tools are called by NAME
          # on the session's PATH; the build's tests launch none of them (fake
          # executor, fake sockets) and those that need sway are ignored.
          cargoBuildFlags = [ "-p" pname ];
          cargoTestFlags = nixpkgs.lib.concatMap (p: [ "-p" p ]) tests;
          meta = {
            inherit description;
            mainProgram = pname;
            license = pkgs.lib.licenses.mit;
            platforms = pkgs.lib.platforms.linux;
          };
        };
    in
    {
      packages = forAll (pkgs: {
        default = self.packages.${pkgs.stdenv.hostPlatform.system}.alpheus;
        alpheus = paquet pkgs {
          pname = "alpheus";
          tests = [ "alpheus" "alpheus-commun" ];
          description = "TUI control center (sound, network, Bluetooth, power, displays, theme)";
        };
        alpheus-demon = paquet pkgs {
          pname = "alpheus-demon";
          tests = [ "alpheus-demon" ];
          description = "alpheus resident daemon: the bar, launcher, notifications and OSD (layer-shell; Hyprland, Sway, Mango and niri)";
          env.ALPHEUS_POLICES = fontDir pkgs;
          # The system bus through dbus-rs: libdbus, found with pkg-config.
          # This nixpkgs' dbus; nixos-config makes the alpheus input follow
          # the system's nixpkgs (inputs.nixpkgs.follows), so the daemon maps
          # the same libdbus store path as the rest of the session (shared pages).
          # libxkbcommon: the launcher's keyboard (sctk's "xkbcommon" feature), found through the same pkg-config.
          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = [ pkgs.dbus pkgs.libxkbcommon ];
        };
        alpheus-launch = paquet pkgs {
          pname = "alpheus-launch";
          tests = [ "alpheus-launch" "alpheus-commun" ];
          description = "Bridge to alpheus's launcher: toggle, close, dmenu-style menus";
        };
      });

      # `nix flake check`: the code must stay formatted (rustfmt.toml).
      checks = forAll (pkgs: {
        fmt = pkgs.runCommand "alpheus-fmt" { nativeBuildInputs = [ pkgs.cargo pkgs.rustfmt ]; } ''
          cd ${./.}
          cargo fmt --all --check
          touch $out
        '';
      });

      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          # sway: the headless compositor of the daemon's ignored tests.
          # dbus: the private dbus-daemon of the daemon's ignored tests (never the session or the system bus).
          # pkg-config, dbus, libxkbcommon: libdbus and the launcher's keyboard, linked by the daemon.
          packages = [
            pkgs.cargo
            pkgs.rustc
            pkgs.clippy
            pkgs.rustfmt
            pkgs.rust-analyzer
            pkgs.sway
            pkgs.dbus
            pkgs.pkg-config
            pkgs.libxkbcommon
            # wtype: virtual keyboard for the launcher's ignored end-to-end tests (headless sway only).
            pkgs.wtype
            # grim (test-only): reads the headless sway's pixels (wlr-screencopy) for the theme tests.
            pkgs.grim
          ];
          ALPHEUS_POLICES = fontDir pkgs;
        };
      });
    };
}
