{
  inputs.basic-dev-shell.url = "github:necauqua/basic-dev-shell";
  outputs = { basic-dev-shell, ... }: basic-dev-shell.make (pkgs: with pkgs; {
    env.LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [
      openssl
      dbus
    ];
    packages = [
      just
      valkey

      rustup
      rustfmt
      clippy
      gcc
      pkg-config

      openssl
      dbus

      nodejs
      wasm-pack
      wasm-bindgen-cli
    ];
  });
}
