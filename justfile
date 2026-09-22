
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

upload-large-reply msg name:
    echo {{quote(msg)}} > /tmp/limatoukka-large-reply.html
    rsync -e 'ssh -o IdentitiesOnly=yes -i ~/.ssh/main-deployer' \
        /tmp/limatoukka-large-reply.html \
        'main-deployer@necauq.ua:limatoukka/{{name}}.html'

aws-tts text voice="Brian":
    echo {{quote(text)}} > /tmp/last-tts.txt
    aws polly synthesize-speech \
        --output-format ogg_vorbis \
        --voice-id "{{voice}}" \
        --text "$(cat /tmp/last-tts.txt)" \
        /tmp/last-tts.ogg
    # mpv \
    #     --no-pause \
    #     --no-terminal \
    #     --ao=jack \
    #     --jack-port="OBS Studio: audio" \
    #     --audio-channels=stereo \
    #     /tmp/last-tts.ogg
    pw-play --volume=0.2 /tmp/last-tts.ogg

@play-sound sound volume="1" cut-start="0" cut-end="0":
    #!/usr/bin/env bash

    duration=$(ffprobe -v error -show_entries format=duration -of default=noprint_wrappers=1:nokey=1 "sounds/{{sound}}")

    read skip_start play_duration < <(awk "BEGIN {
        skip = {{cut-start}} / 1000
        play = {{cut-end}} / 1000
        print skip, (play > 0 ? play : $duration)
    }")

    ffmpeg -ss "$skip_start" -i "sounds/{{sound}}" -t "$play_duration" -f wav - 2>/dev/null | pw-play --volume="{{volume}}" -

### Manual stuff to be run by me:

@when-ping:
    valkey-cli get storage:last-pinger \
      | jq '.timestamp.secs_since_epoch + .next_gate.secs | strflocaltime("%H:%M:%S")' -r
