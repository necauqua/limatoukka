#!/usr/bin/env fish

set -l scene main
set -l source nocapture

set -l itemId (begin
    echo '{"op":1,"d":{"rpcVersion":1}}'
    sleep 0.1
    echo '{"op":6,"d":{"requestType":"GetSceneItemId","requestId":"1","requestData":{"sceneName":"'$scene'","sourceName":"'$source'"}}}'
    sleep 0.1
end | websocat ws://localhost:4455 | jq -sr '.[-1].d.responseData.sceneItemId')

if test -z "$itemId"
    exit 1
end

begin
    echo '{"op":1,"d":{"rpcVersion":1}}'
    sleep 0.1
    set -l rot 0
    while true
        echo '{"op":6,"d":{"requestType":"SetSceneItemTransform","requestId":"1","requestData":{"sceneName":"main","sceneItemId":'$itemId',"sceneItemTransform":{"rotation":'$rot'}}}}'
        set rot (math "($rot + 0.05) % 360")
        sleep 0.005
    end
end | websocat ws://localhost:4455 >/dev/null
