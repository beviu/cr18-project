{ ... }:

{
  programs = {
    clang-format = {
      enable = true;
    };
    nixfmt.enable = true;
    rustfmt.enable = true;
  };
}
