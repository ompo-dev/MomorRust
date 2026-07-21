{ inputs, ... }:
{
  perSystem =
    { pkgs, ... }:
    let
      mkMomor = import ../toolchain.nix { inherit inputs; };
      momor-editor = mkMomor pkgs;
    in
    {
      packages = {
        default = momor-editor;
        debug = momor-editor.override { profile = "dev"; };
      };
    };
}
