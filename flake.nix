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
      paquet = pkgs: { pname, tests, description, env ? { } }:
        pkgs.rustPlatform.buildRustPackage {
          inherit pname env;
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
        };
      });

      devShells = pour (pkgs: {
        default = pkgs.mkShell {
          # sway : compositeur sans tête des tests ignorés du démon.
          packages = [ pkgs.cargo pkgs.rustc pkgs.clippy pkgs.rustfmt pkgs.rust-analyzer pkgs.sway ];
          ALPHEUS_POLICES = polices pkgs;
        };
      });
    };
}
