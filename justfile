
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
    pw-play --volume=0.8 /tmp/last-tts.ogg

play-sound sound volume="1":
    pw-play --volume="{{volume}}" sounds/{{sound}}

[no-exit-message]
@music-np:
    playerctl -p YoutubeMusic metadata -f '{{{{artist}} - {{{{title}}' 2>/dev/null

[no-exit-message]
@music-skip:
    playerctl -p YoutubeMusic next 2>/dev/null

[no-exit-message]
music-queue:
    #!/usr/bin/env bash
    set -euo pipefail

    full=$(curl -s http://localhost:26538/api/v1/queue \
        | jq -c '
          .items[] |
          (.playlistPanelVideoRenderer // .playlistPanelVideoWrapperRenderer.primaryRenderer.playlistPanelVideoRenderer) |
          if .unplayableText != null then
            { videoId, reason: .unplayableText.runs[0].text }
          else
            { title: .title.runs[].text, author: .longBylineText.runs[0].text, videoId, current: .selected }
          end
        ')
    jq -sc '
        to_entries |
        (map(select(.value.current)) | first.key) as $start |
        .[$start:][] |
        .value.idx = .key |
        .value |
        del(.current)
    ' <<< "$full"

[no-exit-message]
music-queue-add videoId:
    #!/usr/bin/env bash
    set -euo pipefail

    if [ -n "$(just music-queue | jq 'select(.videoId == "{{videoId}}")')" ]; then
        echo '{"_already_in_queue":true}'
        exit
    fi

    curl -s http://localhost:26538/api/v1/queue \
        -H 'Content-Type: application/json' \
        -d '{"videoId":"{{videoId}}","insertPosition":"INSERT_AFTER_CURRENT_VIDEO"}'

    sleep 2

    res=$(just music-queue | jq 'select(.videoId == "{{videoId}}")')
    echo -n $res
    if jq -e '.reason' <<< "$res" >/dev/null; then
        curl -s http://localhost:26538/api/v1/queue/$(jq .idx <<< "$res") -X DELETE
    fi

[no-exit-message]
@music-volume volume:
    curl -s http://localhost:26538/api/v1/volume \
        -H 'Content-Type: application/json' \
        -d '{"volume":{{volume}}}'
