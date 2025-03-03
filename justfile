steam-common := "/storage/games/steam/steamapps/common"
proton := "Proton - Experimental"

display := ":17"

export STEAM_COMPAT_CLIENT_INSTALL_PATH := x"~/.local/share/Steam"
export STEAM_COMPAT_DATA_PATH := justfile_dir() + "/noita/steam-compat-data"

_default:
    @just -l

# Start the Noita instance
[working-directory: 'noita']
launch:
    # idempotently make sure things are in place
    mkdir -p "noita/steam-compat-data"
    # just link .exe, .dll and data into a new cwd ¯\_(ツ)_/¯
    ln -sf "{{steam-common}}/Noita/"{*.dll,noita.exe,data} noita/
    # stop release notes popup (config must have the same hash string)
    echo none > noita/_version_hash.txt

    # and just run it lol
    # so we wrap the noita.exe in proton to run it on linux,
    # wrap that in steam-run to run it on NixOS,
    # and wrap _that_ in vglrun to make it be able to use the gpu from another X instance
    vglrun steam-run \
        "{{steam-common}}/{{proton}}/proton" \
        waitforexitandrun \
        noita.exe \
        -- \
        -no_logo_splashes \
        -gamemode

save-dir := "noita/steam-compat-data/pfx/drive_c/users/steamuser/AppData/LocalLow/Nolla_Games_Noita"

# Delete the save (but set "intro_has_played" tag)
reset:
    rm -rf noita
    mkdir -p "{{save-dir}}/"{save_shared,save00/persistent/flags}
    touch "{{save-dir}}/save00/persistent/flags/intro_has_played"
    ln config.xml "{{save-dir}}/save_shared/config.xml"

# Force the XSH display capture input to reconnect to the X instance
obs-reset:
    (\
    echo '{"op":1,"d":{"rpcVersion":1}}';\
    sleep 0.1;\
    echo '{"op":6,"d":{"requestType":"SetInputSettings","requestId":"1","requestData":{"inputName":"capture","inputSettings":{"server":":99"}}}}';\
    echo '{"op":6,"d":{"requestType":"SetInputSettings","requestId":"1","requestData":{"inputName":"capture","inputSettings":{"server":"{{display}}"}}}}';\
    ) | websocat ws://localhost:4455

# Start the game in a separate X instance
run:
    #!/usr/bin/env bash
    function cleanup() {
        # so that the last frame is not frozen
        just obs-reset || true
    }
    trap cleanup SIGINT

    xdummy {{display}} &
    sleep 0.1

    # hide stupid X cursor when the game is not running
    DISPLAY={{display}} xsetroot -cursor none.xbm none.xbm

    # make sure obs capture is connected to this instance
    just obs-reset || true

    # and just start the game now
    DISPLAY={{display}} just launch

stop:
    # bit risky, lol
    pgrep X | tail -1 | xargs kill
    just obs-reset || true
