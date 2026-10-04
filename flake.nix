{
  description = "alpheus — centre de contrôle TUI de Mae";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systemes = [ "x86_64-linux" "aarch64-linux" ];
      pour = f: nixpkgs.lib.genAttrs systemes (systeme: f nixpkgs.legacyPackages.${systeme});
    in
    {
      packages = pour (pkgs: {
        default = self.packages.${pkgs.stdenv.hostPlatform.system}.alpheus;

        alpheus = pkgs.rustPlatform.buildRustPackage {
          pname = "alpheus";
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          # Workspace : ne construire que la TUI ; tester la TUI et le socle.
          cargoBuildFlags = [ "-p" "alpheus" ];
          cargoTestFlags = [ "-p" "alpheus" "-p" "alpheus-commun" ];
          # Les outils de session (wpctl, nmcli…) sont appelés par NOM dans
          # le PATH de la session : pas de wrapProgram, ce sont ceux du poste.
          # Les tests du build n'en lancent aucun (faux exécuteur).
          meta = {
            description = "Centre de contrôle TUI (son, réseau, Bluetooth, énergie, alimentation, écrans)";
            mainProgram = "alpheus";
            platforms = pkgs.lib.platforms.linux;
          };
        };
      });

      devShells = pour (pkgs: {
        default = pkgs.mkShell {
          packages = [ pkgs.cargo pkgs.rustc pkgs.clippy pkgs.rustfmt pkgs.rust-analyzer ];
          # Polices du démon (JetBrainsMono Nerd Font) : lues par les tests de dessin.
          ALPHEUS_POLICES = "${pkgs.nerd-fonts.jetbrains-mono}/share/fonts/truetype/NerdFonts/JetBrainsMono";
        };
      });
    };
}
