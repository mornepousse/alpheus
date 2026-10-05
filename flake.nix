{
  description = "alpheus — centre de contrôle TUI et démon résident (barre) de Mae";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systemes = [ "x86_64-linux" "aarch64-linux" ];
      pour = f: nixpkgs.lib.genAttrs systemes (systeme: f nixpkgs.legacyPackages.${systeme});
      # Police par défaut du démon (JetBrainsMono Nerd Font) : dossier figé à
      # la compilation (option_env!) et lu par ses tests. Pas de wrapProgram :
      # il renommerait le processus (.alpheus-demon-wrapped) et
      # `pkill -x alpheus-demon` ne le trouverait plus (même piège que waybar).
      polices = pkgs: "${pkgs.nerd-fonts.jetbrains-mono}/share/fonts/truetype/NerdFonts/JetBrainsMono";
      # nativeBuildInputs / buildInputs: system libraries a package links
      # (the daemon's libxkbcommon, launcher plan task 12); empty by default.
      paquet = pkgs: { pname, tests, description, env ? { }, nativeBuildInputs ? [ ], buildInputs ? [ ] }:
        pkgs.rustPlatform.buildRustPackage {
          inherit pname env nativeBuildInputs buildInputs;
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          # Workspace : un paquet par binaire. Les outils de session sont
          # appelés par NOM dans le PATH de la session ; les tests du build
          # n'en lancent aucun (faux exécuteur, faux sockets) et ceux qui
          # demandent sway sont ignorés.
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
      packages = pour (pkgs: {
        default = self.packages.${pkgs.stdenv.hostPlatform.system}.alpheus;
        alpheus = paquet pkgs {
          pname = "alpheus";
          tests = [ "alpheus" "alpheus-commun" ];
          description = "Centre de contrôle TUI (son, réseau, Bluetooth, énergie, alimentation, écrans)";
        };
        alpheus-demon = paquet pkgs {
          pname = "alpheus-demon";
          tests = [ "alpheus-demon" ];
          description = "Démon résident d'alpheus : la barre (layer-shell, Hyprland et niri)";
          env.ALPHEUS_POLICES = polices pkgs;
          # libxkbcommon: the launcher's keyboard (sctk's "xkbcommon" feature), found through pkg-config.
          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = [ pkgs.libxkbcommon ];
        };
        alpheus-launch = paquet pkgs {
          pname = "alpheus-launch";
          tests = [ "alpheus-launch" "alpheus-commun" ];
          description = "Bridge to alpheus's launcher: toggle, close, dmenu-style menus";
        };
      });

      # `nix flake check`: the code must stay formatted (rustfmt.toml).
      checks = pour (pkgs: {
        fmt = pkgs.runCommand "alpheus-fmt" { nativeBuildInputs = [ pkgs.cargo pkgs.rustfmt ]; } ''
          cd ${./.}
          cargo fmt --all --check
          touch $out
        '';
      });

      devShells = pour (pkgs: {
        default = pkgs.mkShell {
          # sway : compositeur sans tête des tests ignorés du démon.
          # dbus : dbus-daemon privé des tests ignorés du démon (jamais le bus de session ni le bus système).
          # pkg-config, libxkbcommon: the daemon links libxkbcommon (launcher keyboard).
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
          ];
          ALPHEUS_POLICES = polices pkgs;
        };
      });
    };
}
