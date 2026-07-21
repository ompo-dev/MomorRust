{ inputs, ... }:
{
  flake.overlays.default =
    final: _:
    let
      mkMomor = import ../toolchain.nix { inherit inputs; };
    in
    {
      momor-editor = mkMomor final;
    };
}
