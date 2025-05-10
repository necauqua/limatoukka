steam-common := "/storage/games/steam/steamapps/common"
proton := "Proton - Experimental"

display := ":17"
noita-dir := "noita"
compat-dir := "noita/steam-compat-data"

export STEAM_COMPAT_CLIENT_INSTALL_PATH := x"~/.local/share/Steam"
export STEAM_COMPAT_DATA_PATH := justfile_dir() + "/" + compat-dir

_default:
    @just -l

save-dir := compat-dir + "/pfx/drive_c/users/steamuser/AppData/LocalLow/Nolla_Games_Noita"

# Start the Noita instance
start:
    # idempotently make sure things are in place:
    mkdir -p "{{save-dir}}/"{save_shared,save00/persistent/flags}
    ln -f config.xml "{{save-dir}}/save_shared/config.xml"
    # just link .exe, .dll and data into a new cwd ¯\_(ツ)_/¯
    ln -sf "{{steam-common}}/Noita/"{*.dll,noita.exe,data} noita
    # stop release notes popup (config must have the same hash string)
    echo -n static > {{noita-dir}}/_version_hash.txt
    # just run it lol
    # so we wrap the noita.exe in proton to run it on linux,
    # wrap that in steam-run to run it on NixOS,
    # and wrap _that_ in vglrun to make it be able to use the gpu from another X instance
    cd noita && steam-run \
        "{{steam-common}}/{{proton}}/proton" \
        waitforexitandrun \
        noita.exe \
        -- \
        -no_logo_splashes \
        -gamemode >/dev/null 2>/dev/null

# Completely delete the instance, including stats, unlocks etc.
full-reset:
    rm -rf noita

# Deletes the world data
reset:
    rm -rf {{save-dir}}/save00/{world,player.xml,world_state.xml,session_numbers.salakieli}

# Set an arbitrary persistent flag
set-flag flag:
    mkdir -p "{{save-dir}}/save00/persistent/flags"
    touch "{{save-dir}}/save00/persistent/flags/{{flag}}"

# Set the intro_has_played flag
no-intro:
    just set-flag intro_has_played

# Start the game in a separate X instance
run:
    #!/usr/bin/env bash
    # function cleanup() {
    #     # so that the last frame is not frozen
    #     just obs-reset-display
    #     # just stop
    # }
    # trap cleanup INT TERM EXIT

    xdummy {{display}} 2>/dev/null &
    sleep 0.1

    export DISPLAY={{display}}

    # hide stupid X cursor when the game is not running
    xsetroot -cursor none.xbm none.xbm

    # set root color to magenta to chromakey the nocapture thing below the thing
    xsetroot -solid "#ff00ff"

    # make sure obs capture is connected to this instance
    just obs-reset-display

    just sound-setup &

    # and just start the game now, in that instance
    vglrun just --color=always start 2> >(grep -v "wrong ELF class: ELFCLASS32" >&2)

stop:
    #!/usr/bin/env bash
    DISPLAY={{display}} xdotool key Alt+F4
    while pgrep noita.exe >/dev/null; do
        sleep 0.1
    done
    pkill .exe
    pkill wine
    pkill -f pipewire-obs-thing.lua
    # this will fail to kill the main X instance, pfew
    pgrep X | tail -1 | xargs kill
    just obs-reset-display

restart:
    #!/usr/bin/env bash
    ./obs-files/hide-nocap.fish &
    sleep 0.2
    just stop
    sleep 2
    just run

reset-restart:
    #!/usr/bin/env bash
    ./obs-files/hide-nocap.fish &
    sleep 0.2
    just stop reset
    sleep 2
    just run

sound-setup:
    wpexec pipewire-obs-thing.lua '{"display":"{{display}}"}'

# Force the XSH display capture input to reconnect to the X instance
obs-reset-display:
    @(\
    echo '{"op":1,"d":{"rpcVersion":1}}'; \
    sleep 0.1; \
    echo '{"op":6,"d":{"requestType":"SetInputSettings","requestId":"1","requestData":{"inputName":"capture","inputSettings":{"server":":99"}}}}'; \
    sleep 0.01; \
    echo '{"op":6,"d":{"requestType":"SetInputSettings","requestId":"2","requestData":{"inputName":"capture","inputSettings":{"server":"{{display}}"}}}}'; \
    ) | websocat ws://localhost:4455 >/dev/null

obs-refresh-chat:
    @(\
    echo '{"op":1,"d":{"rpcVersion":1}}'; \
    sleep 0.1; \
    echo '{"op":6,"d":{"requestType":"PressInputPropertiesButton","requestId":"1","requestData":{"inputName":"chat message","propertyName":"refreshnocache"}}}'; \
    ) | websocat ws://localhost4455 >/dev/null

[working-directory("obs-files")]
start-intro-timer seconds="900":
    just stop-intro-timer 2>/dev/null || true
    ./countdown.fish "{{seconds}}" & disown

stop-intro-timer:
    pkill -f ./countdown.fish

start-funny-rotation:
    ./obs-files/rotate-lol.fish & disown

stop-funny-rotation:
    pkill -f ./rotate-lol.fish

cringe-scp-large-reply msg:
    echo {{quote(msg)}} > /tmp/last-reply.html
    rsync -e 'ssh -o IdentitiesOnly=yes -i ~/.ssh/main-deployer' /tmp/last-reply.html main-deployer@necauq.ua:.
