#!/usr/bin/env fish

function on_sigint --on-process-exit=%self
    echo "starting soon" >starting-in.txt
end

set time 900

if [ ! -z "$argv[1]" ]
    set time "$argv[1]"
end

while [ "$time" -gt 0 ]
    echo "starting in $(date "-d@$time" -u +%M:%S)" >starting-in.txt
    set time (math $time - 1)
    sleep 1
end

echo "starting soon" >starting-in.txt
