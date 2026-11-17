# Nix and NixOS

## Nix and NixOS

```sh
nix run github:k4ditano/pleamar -- --scene bar.plm
```

As a flake input (`pleamar.url = "github:k4ditano/pleamar"`): the package
(`pleamar.packages.${system}.default`, or `pkgs.pleamar` through
`pleamar.overlays.default`) and a home-manager module:

```nix
imports = [ inputs.pleamar.homeManagerModules.default ];
programs.pleamar = {
  enable = true;
  autostart = true;   # runs ~/.config/pleamar/autostart with your graphical session
};
```

The whole desktop (pleamar-wm with Marea, in your login screen) is
[pleamar-wm]'s NixOS module. With Nix on another distribution, graphical Nix
programs need [nixGL] to reach your GPU driver.


[nixGL]: https://github.com/nix-community/nixGL
[pleamar-wm]: https://github.com/k4ditano/pleamar-wm
