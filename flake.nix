{
  inputs.basic-dev-shell.url = "github:necauqua/basic-dev-shell";
  outputs = { basic-dev-shell, ... }: basic-dev-shell.make (pkgs: with pkgs;
    let
      xdummy-conf = writeText "xdummy.conf" ''
        # Based on https://github.com/Xpra-org/xpra/blob/master/docs/Usage/Xdummy.md

        Section "ServerFlags"
          Option "DontVTSwitch" "true"
          Option "AllowMouseOpenFail" "true"
          Option "PciForceNone" "true"
          Option "AllowEmptyInput" "true"
          Option "AutoEnableDevices" "false"
          Option "AutoAddDevices" "false"
        EndSection

        Section "Device"
          Identifier "dummy_videocard"
          Driver "dummy"
          Option "ConstantDPI" "true"
          VideoRam 768000
        EndSection

        Section "Monitor"
          Identifier "dummy_monitor"
          HorizSync   1.0 - 300000.0
          VertRefresh 1.0 - 300.0
          Modeline "1920x1080" 23.53 1920 1952 2040 2072 1080 1106 1108 1135
        EndSection

        Section "Screen"
          Identifier "dummy_screen"
          Device "dummy_videocard"
          Monitor "dummy_monitor"
          DefaultDepth 24
          SubSection "Display"
            Viewport 0 0
            Depth 24
            Modes "1920x1080"
            Virtual 1920 1080
          EndSubSection
        EndSection

        Section "ServerLayout"
          Identifier   "dummy_layout"
          Screen       "dummy_screen"
        EndSection

        Section "Files"
          ModulePath "${xorg.xf86videodummy}/lib/xorg/modules/drivers"
        EndSection
      '';
      xdummy = pkgs.writeShellScriptBin "xdummy" ''
        exec ${xorg.xorgserver}/bin/X \
          -noreset \
          -nocursor \
          +extension GLX \
          +extension RANDR \
          +extension RENDER \
          -logfile ./xdummy.log \
          -config ${xdummy-conf} \
          $@
      '';
      mpv = pkgs.mpv-unwrapped.override { jackaudioSupport = true; };
    in
    {
      env.LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [
        pkgsi686Linux.pulseaudio # for audio to work
        pipewire.jack # for OBS audio to work 🤦
        openssl
        dbus
        xorg.libX11
        xdotool
      ];
      packages = [
        xdummy
        pkgsi686Linux.virtualglLib
        just
        websocat
        hugo
        go
        valkey

        rustup
        rustfmt
        clippy
        gcc
        pkg-config

        openssl
        dbus
        xdotool
        mpv
        xorg.libX11

        nodejs
        wasm-pack
        wasm-bindgen-cli
      ];
    });
}
