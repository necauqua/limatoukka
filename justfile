
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

aws-tts text voice="Brian" engine="standard":
    echo {{quote(text)}} > /tmp/last-tts.txt
    aws polly synthesize-speech \
        --output-format ogg_vorbis \
        --voice-id "{{voice}}" \
        --engine "{{engine}}" \
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

top-charges:
    #!/usr/bin/env fish

    set -l keys (valkey-cli --scan --pattern 'charges:*')
    set -l uids (string replace 'charges:' '' $keys)
    set -l vals (valkey-cli mget $keys)
    set -l total (count $uids)

    # names.rs caches twitch display names in valkey under `caches:names:<uid>`
    # (24h TTL). Reuse that cache and only call twitch for the misses, writing
    # freshly-resolved names back with the same SET NX PX semantics.
    set -l names_ttl 86400000 # 24h in ms

    begin
        set -l cold
        for u in $uids
            set -l name (valkey-cli get "caches:names:$u")
            if test -n "$name"
                printf 'N\t%s\t%s\n' $u $name
            else
                set -a cold $u
            end
        end

        set -l ncold (count $cold)
        for i in (seq 1 100 $ncold)
            set -l last (math "min($i + 99, $ncold)")
            set -l args
            for u in $cold[$i..$last]
                set -a args -q id=$u
            end
            twitch-cli api get users $args | jq -r '.data[] | "\(.id)\t\(.display_name)"' | while read -l id dn
                valkey-cli set "caches:names:$id" "$dn" PX $names_ttl NX >/dev/null
                printf 'N\t%s\t%s\n' $id $dn
            end
        end

        for i in (seq 1 $total)
            printf 'V\t%s\t%s\n' $uids[$i] $vals[$i]
        end
    end | awk -F'\t' '
        # same rendering as the Display impl of Charges in src/services/charges.rs:
        # charges are stored as thousandths, trailing fraction zeroes are dropped
        function fmt(v,   sig, whole, frac) {
            v = v + 0
            sig = v < 0 ? "-" : ""
            if (v < 0) v = -v
            whole = int(v / 1000)
            frac = sprintf("%03d", v % 1000)
            sub(/0+$/, "", frac)
            return sig whole (frac == "" ? "" : "." frac)
        }
        $1 == "N" { name[$2] = $3; next }
        {
            who = name[$2] ? name[$2] : $2
            # the bot itself is the burn destination, it is outside of the economy
            if (who == "Limatoukka") next
            printf "%d\t%s\t%s\n", $3 + 0, fmt($3), who
        }
    ' | sort -rn | awk -F'\t' '
        # buffer everything to right-align the amounts; the bolt comes after the
        # padding so that the terminal width of the symbol does not matter
        {
            amount[NR] = $2
            who[NR] = $3
            if (length($2) > w) w = length($2)
        }
        END {
            for (i = 1; i <= NR; i++) printf "%*s⚡︎  %s\n", w, amount[i], who[i]
        }
    ' | less
