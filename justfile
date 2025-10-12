
_default:
    @just -ul

[no-exit-message]
is-game-running:
    #!/usr/bin/env bash
    for pid in $(pgrep -f noita.exe); do
        exit 0
        # if cat /proc/$pid/environ 2>/dev/null | rg -q TWITCH_PLAYS_NOITA=1 ; then
        #     exit 0
        # fi
    done
    exit 1

upload-large-reply msg:
    echo {{quote(msg)}} > /tmp/last-reply.html
    rsync -e 'ssh -o IdentitiesOnly=yes -i ~/.ssh/main-deployer' /tmp/last-reply.html main-deployer@necauq.ua:.

aws-tts text:
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
    pw-play --volume=0.4 /tmp/last-tts.ogg

play-sound sound volume="1":
    pw-play --volume="{{volume}}" sounds/{{sound}}
