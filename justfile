
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

[no-exit-message]
@music-np:
    playerctl -p YoutubeMusic metadata -f '{{{{artist}} - {{{{title}}' 2>/dev/null

[no-exit-message]
@music-skip:
    playerctl -p YoutubeMusic next 2>/dev/null

[no-exit-message]
music-queue pos="0":
    #!/usr/bin/env bash
    set -euo pipefail

    full=$(curl -s http://localhost:26538/api/v1/queue \
        | jq -c '
          .items[] |
          (.playlistPanelVideoRenderer // .playlistPanelVideoWrapperRenderer.primaryRenderer.playlistPanelVideoRenderer) |
          if .unplayableText != null then
            { videoId, _broken: true }
          else
            { title: .title.runs[].text, author: .longBylineText.runs[0].text, videoId, current: .selected }
          end
        ')
    jq -sc '
        to_entries |
        (map(select(.value.current)) | first.key) as $start |
        .[:{{pos}} + 1][$start + 1:][] |
        .value.idx = .key |
        .value |
        del(.current)
    ' <<< "$full"

[no-exit-message]
music-queue-add videoId pos="0":
    #!/usr/bin/env bash
    set -euo pipefail

    if [ -n "$(just music-queue {{pos}} | jq 'select(.videoId == "{{videoId}}")')" ]; then
        echo '{"_already_in_queue":true}'
        exit
    fi

    # try to avoid issues at the transition point
    if [ "$(curl -s http://localhost:26538/api/v1/song | jq '.songDuration - .elapsedSeconds')" -le 2 ]; then
        sleep 3
        exit
    fi

    curl -s http://localhost:26538/api/v1/queue \
        -H 'Content-Type: application/json' \
        -d '{"videoId":"{{videoId}}","insertPosition":"INSERT_AFTER_CURRENT_VIDEO"}'

    sleep 2

    res=$(just music-queue 999999 | jq 'select(.videoId == "{{videoId}}")')
    idx=$(jq .idx <<< "$res")
    if jq -e '._broken' <<< "$res" >/dev/null; then
        curl -s "http://localhost:26538/api/v1/queue/$idx" -X DELETE
        echo '{"_broken":true}'
        exit
    fi

    if [ "{{pos}}" -gt "$idx" ]; then
        curl -s "http://localhost:26538/api/v1/queue/$idx" -X PATCH \
            -H 'Content-Type: application/json' \
            -d '{"toIndex":'"{{pos}}"'}'
        echo $res | jq -c '.idx = '"{{pos}}"''
    else
        echo -n $res
    fi
