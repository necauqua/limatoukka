steam-common := "/storage/games/steam/steamapps/common"
proton := "Proton - Experimental"

display := ":17"
noita-dir := "noita"
compat-dir := "noita/steam-compat-data"

export STEAM_COMPAT_CLIENT_INSTALL_PATH := x"~/.local/share/Steam"
export STEAM_COMPAT_DATA_PATH := justfile_dir() + "/" + compat-dir

export TWITCH_PLAYS_NOITA := "1"

_default:
    @just -l

save-dir := compat-dir + "/pfx/drive_c/users/steamuser/AppData/LocalLow/Nolla_Games_Noita"

# Start the Noita instance
start mode="0" seed="":
    #!/usr/bin/env bash
    set -euo pipefail

    # idempotently make sure things are in place:
    mkdir -p "{{save-dir}}/"{save_shared,save00/persistent/flags}
    ln -sf "{{save-dir}}/save00" "{{noita-dir}}/save00"
    ln -f config.xml "{{save-dir}}/save_shared/config.xml"
    # just link .exe, .dll and data into a new cwd ¯\_(ツ)_/¯
    ln -sf "{{steam-common}}/Noita/"{*.dll,noita.exe,data} noita
    # stop release notes popup (config must have the same hash string)
    echo -n static > {{noita-dir}}/_version_hash.txt

    if [ -n "{{seed}}" ]; then
        sed -ri "s/seed = [0-9]+/seed = {{seed}}/g" {{noita-dir}}/mods/tpn-seed/init.lua
        sed -i 's/enabled="0" name="tpn-seed"/enabled="1" name="tpn-seed"/g' {{save-dir}}/save00/mod_config.xml
    else
        sed -i 's/enabled="1" name="tpn-seed"/enabled="0" name="tpn-seed"/g' {{save-dir}}/save00/mod_config.xml
    fi

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
        -gamemode {{mode}} &

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

setup-x:
    #!/usr/bin/env bash
    xdummy {{display}} 2>/dev/null &
    sleep 0.1

    # hide stupid X cursor when the game is not running
    env DISPLAY={{display}} xsetroot -cursor none.xbm none.xbm

    # set root color to magenta to chromakey the nocapture thing below the thing
    env DISPLAY={{display}} xsetroot -solid "#ff00ff"

    # make sure obs capture is connected to this instance
    just obs-reset-display

# Start the game in a separate X instance
run mode="0" seed="":
    # just start the game on the display from the setup-x step
    env DISPLAY={{display}} vglrun just --color=always start {{mode}} {{seed}} 2> >(grep -v "wrong ELF class: ELFCLASS32" >&2)

stop:
    #!/usr/bin/env bash
    DISPLAY={{display}} xdotool key Alt+F4
    while just is-game-running; do
        sleep 0.1
    done

restart mode="0" seed="":
    #!/usr/bin/env bash
    ./obs-files/hide-nocap.fish &
    sleep 0.2
    just stop
    sleep 2
    just run {{mode}} {{seed}}

reset-restart mode="0" seed="":
    #!/usr/bin/env bash
    ./obs-files/hide-nocap.fish &
    sleep 0.2
    just stop reset run {{mode}} {{seed}}

sound-setup:
    #!/usr/bin/env bash
    pkill -f pipewire-obs-thing.lua
    wpexec pipewire-obs-thing.lua '{"display":"{{display}}"}' >/dev/null 2>&1 </dev/null &

# Force the XSH display capture input to reconnect to the X instance
obs-reset-display:
    @(\
    echo '{"op":1,"d":{"rpcVersion":1}}'; \
    sleep 0.1; \
    echo '{"op":6,"d":{"requestType":"SetInputSettings","requestId":"1","requestData":{"inputName":"capture","inputSettings":{"server":":99"}}}}'; \
    sleep 0.01; \
    echo '{"op":6,"d":{"requestType":"SetInputSettings","requestId":"2","requestData":{"inputName":"capture","inputSettings":{"server":"{{display}}"}}}}'; \
    ) | websocat ws://localhost:4455 >/dev/null

obs-refresh input="chat message":
    @(\
    echo '{"op":1,"d":{"rpcVersion":1}}'; \
    sleep 0.1; \
    echo '{"op":6,"d":{"requestType":"PressInputPropertiesButton","requestId":"1","requestData":{"inputName":"{{input}}","propertyName":"refreshnocache"}}}'; \
    ) | websocat ws://localhost:4455 >/dev/null

obs-stop-stream:
    @(\
    echo '{"op":1,"d":{"rpcVersion":1}}'; \
    sleep 0.1; \
    echo '{"op":6,"d":{"requestType":"StopStream","requestId":"1"}}'; \
    ) | websocat ws://localhost:4455 >/dev/null

obs-start-stream:
    @(\
    echo '{"op":1,"d":{"rpcVersion":1}}'; \
    sleep 0.1; \
    echo '{"op":6,"d":{"requestType":"StartStream","requestId":"1"}}'; \
    ) | websocat ws://localhost:4455 >/dev/null

obs-revive:
    #!/usr/bin/env bash
    # ughh, nixos wrappers
    if ! pgrep -x .obs-wrapped >/dev/null; then
        obs --disable-shutdown-check >/dev/null 2>&1 </dev/null &
        sleep 4
        just sound-setup obs-start-stream
        echo true
    fi

[no-exit-message]
is-game-running:
    #!/usr/bin/env bash
    for pid in $(pgrep -f noita.exe); do
        if cat /proc/$pid/environ 2>/dev/null | rg -q TWITCH_PLAYS_NOITA=1 ; then
            exit 0
        fi
    done
    exit 1

[working-directory("obs-files")]
start-intro-timer seconds="900":
    just stop-intro-timer 2>/dev/null || true
    ./countdown.fish "{{seconds}}" & disown

stop-intro-timer:
    pkill -f ./countdown.fish

cringe-scp-large-reply msg:
    echo {{quote(msg)}} > /tmp/last-reply.html
    rsync -e 'ssh -o IdentitiesOnly=yes -i ~/.ssh/main-deployer' /tmp/last-reply.html main-deployer@necauq.ua:.

cringe-aws-tts-through-shell text:
    echo {{quote(text)}} > /tmp/last-tts.txt
    aws polly synthesize-speech \
        --output-format ogg_vorbis \
        --voice-id Brian \
        --text "$(cat /tmp/last-tts.txt)" \
        /tmp/last-tts.ogg
    # mpv \
    #     --no-pause \
    #     --no-terminal \
    #     --ao=jack \
    #     --jack-port="OBS Studio: audio" \
    #     --audio-channels=stereo \
    #     /tmp/last-tts.ogg
    mpv \
        --no-pause \
        --no-terminal \
        --audio-channels=stereo \
        /tmp/last-tts.ogg
    echo true
