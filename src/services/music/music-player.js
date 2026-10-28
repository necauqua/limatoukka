let player;

// the server has the true values, these are only copies
let globalVolume = 15;
// volume for the current song only, null for the global volume
let songVolume = null;

// local files are louder than most YouTube music: they play at this
// part of the slider volume (slider 18 plays as 10)
const FILE_VOLUME_SCALE = 10 / 18;

// the two ways to play a song (the `kind` of a playback from the
// server): the YouTube player and the audio element
const backends = {
    youtube: {
        load({ videoId }, start, paused) {
            const options = { videoId, startSeconds: start };
            if (paused) {
                player.cueVideoById(options);
            } else {
                player.loadVideoById(options);
            }
        },
        stop: () => player?.stopVideo?.(),
        play: () => player?.playVideo(),
        pause: () => player?.pauseVideo(),
        isPlaying: () =>
            player?.getPlayerState?.() === YT.PlayerState.PLAYING,
        time: () => player?.getCurrentTime?.() ?? 0,
        duration: () => player?.getDuration?.() ?? 0,
        seek: (time) => player?.seekTo(time, true),
    },
    file: {
        load({ url }, start, paused) {
            audio.src = url;
            audio.currentTime = start;
            if (paused) {
                pause.innerText = "▶︎";
            } else {
                audio.play().catch(console.error);
            }
        },
        stop() {
            audio.pause();
            audio.removeAttribute("src");
            audio.load();
        },
        play: () => audio.play().catch(console.error),
        pause: () => audio.pause(),
        isPlaying: () => !audio.paused,
        time: () => audio.currentTime,
        duration: () => (Number.isFinite(audio.duration) ? audio.duration : 0),
        seek: (time) => (audio.currentTime = time),
    },
};

// the backend that plays the current song
let backend = null;

// show the volume and give it to the players; the slider shows
// the volume that plays now
function applyVolume() {
    const effective = songVolume ?? globalVolume;
    volume.value = effective;
    volumeDisplay.innerText =
        songVolume === null
            ? globalVolume
            : globalVolume + " (" + songVolume + ")";
    player?.setVolume?.(effective);
    audio.volume = (effective * FILE_VOLUME_SCALE) / 100;
}

// while the slider is dragged
function volumeInput() {
    const value = parseInt(volume.value);
    if (localVolume.checked) {
        songVolume = value;
    } else {
        globalVolume = value;
        songVolume = null;
    }
    applyVolume();
}

// when the slider is released
function volumeChange() {
    volumeInput();
    if (localVolume.checked) {
        localVolume.checked = false;
        fetch("/state/song-volume/" + songVolume, {
            method: "POST",
        }).catch(console.error);
    } else {
        fetch("/state/volume/" + globalVolume, {
            method: "POST",
        }).catch(console.error);
    }
}

function showTitle(current) {
    if (!current) {
        currentTitle.textContent = "";
        return;
    }
    const {
        song: { title, author },
        source,
    } = current;
    currentTitle.textContent = title + " - " + author;
    if (source) {
        currentTitle.textContent +=
            " (requested by " + source.requester + ")";
    }
}

// what the backend plays, its position and duration, to resume
// after a reload of the page
let currentKey = null;
// a cued video has no duration in the player until it plays
let currentDuration = 0;

function playbackKey(playback) {
    return playback.kind + ":" + (playback.videoId ?? playback.url);
}

function duration() {
    const fromBackend = backend?.duration() ?? 0;
    if (fromBackend > 0) {
        currentDuration = fromBackend;
    }
    return currentDuration;
}

function showPosition(time) {
    const total = duration();
    seek.value =
        total > 0 ? Math.min(1000, (time * 1000) / total) : 0;
}

function savePosition() {
    if (currentKey && backend) {
        localStorage.setItem(
            "position",
            JSON.stringify({
                key: currentKey,
                time: backend.time(),
                duration: duration(),
            }),
        );
    }
}

function savedPosition(key) {
    try {
        const saved = JSON.parse(localStorage.getItem("position"));
        if (saved?.key === key) {
            return {
                time: saved.time,
                duration: saved.duration ?? 0,
            };
        }
    } catch {}
    return { time: 0, duration: 0 };
}

// give the song to its backend, and stop the other one
function load({ song, playback }, start, paused) {
    const next = backends[playback.kind];
    if (backend && backend !== next) {
        backend.stop();
    }
    backend = next;
    currentKey = playbackKey(playback);
    // songs from a playlist have no length
    currentDuration = song.length?.secs ?? 0;
    backend.load(playback, start, paused);
}

let resolvePlayerReady;
const playerReady = new Promise((r) => (resolvePlayerReady = r));

// load the current song from the server, at the saved position
async function resume(current, paused) {
    if (current?.playback.kind === "youtube") {
        await playerReady;
    }
    if (!current || currentKey) {
        return;
    }
    const saved = savedPosition(playbackKey(current.playback));
    load(current, saved.time, paused);
    if (saved.duration > 0) {
        currentDuration = saved.duration;
    }
    showPosition(saved.time);
}

let firstLoad = true;

async function loadState() {
    const res = await fetch("/state");
    const { current, paused, volume } = await res.json();
    globalVolume = volume.global;
    songVolume = volume.song;
    applyVolume();
    showTitle(current);
    if (firstLoad) {
        firstLoad = false;
        await resume(current, paused);
    }
}

function setPaused(paused) {
    pause.innerText = paused ? "▶︎" : "⏸︎";
    fetch("/state/paused/" + paused, { method: "POST" }).catch(
        console.error,
    );
}

let seekUpdater = null;

function stopSeekUpdater() {
    if (seekUpdater) {
        clearInterval(seekUpdater);
        seekUpdater = null;
    }
}

// only playing and paused change the state on the server;
// the other states happen on each song change
function onPlaying() {
    setPaused(false);
    stopSeekUpdater();
    seekUpdater = setInterval(() => {
        showPosition(backend.time());
        savePosition();
    }, 200);
}

function onPaused() {
    stopSeekUpdater();
    savePosition();
    setPaused(true);
}

function onEnded() {
    stopSeekUpdater();
    playNext().catch(console.error);
}

function updateSeek() {
    backend?.seek((seek.value * duration()) / 1000);
}

// called by the yt iframe api
function onYouTubeIframeAPIReady() {
    player = new YT.Player("player", {
        height: "390",
        width: "640",
        events: {
            onReady() {
                applyVolume();
                resolvePlayerReady();
            },
            onStateChange(e) {
                // also when it stops because a file plays now
                if (backend !== backends.youtube) {
                    return;
                }
                applyVolume();
                switch (e.data) {
                    case YT.PlayerState.PLAYING:
                        onPlaying();
                        break;
                    case YT.PlayerState.PAUSED:
                        onPaused();
                        break;
                    case YT.PlayerState.CUED:
                        pause.innerText = "▶︎";
                        break;
                    case YT.PlayerState.ENDED:
                        onEnded();
                        break;
                }
            },
        },
    });
}

audio.addEventListener("playing", () => {
    if (backend === backends.file) {
        onPlaying();
    }
});
audio.addEventListener("pause", () => {
    // not at the end of a song, and not when the source changes
    if (
        backend === backends.file &&
        !audio.ended &&
        audio.readyState >= audio.HAVE_CURRENT_DATA
    ) {
        onPaused();
    }
});
audio.addEventListener("ended", () => {
    if (backend === backends.file) {
        onEnded();
    }
});
audio.addEventListener("error", () => {
    if (backend === backends.file && audio.getAttribute("src")) {
        console.error("cannot play", audio.src, audio.error);
        // after a delay, in case no file plays
        setTimeout(() => playNext().catch(console.error), 1000);
    }
});

async function play(endpoint) {
    const res = await fetch(endpoint, { method: "POST" }).then(
        (res) => res.json(),
    );
    stopSeekUpdater();
    seek.value = 0;
    // the server removes the song volume on /next and /prev
    songVolume = null;
    applyVolume();
    showTitle(res);
    if (res) {
        if (res.playback.kind === "youtube") {
            await playerReady;
        }
        load(res, 0, false);
    }
}

function togglePause() {
    if (backend?.isPlaying()) {
        backend.pause();
    } else {
        backend?.play();
    }
}

async function playPrev() {
    await play("/prev");
}

async function playNext() {
    await play("/next");
}

async function reset() {
    await fetch("/reset", { method: "POST" });
}

live("/events", {
    // on each (re)connect, and when the page missed changes
    state: () => loadState().catch(console.error),
    // the server already stored the values of these two events
    volume: (data) => {
        globalVolume = parseInt(data);
        songVolume = null;
        applyVolume();
    },
    "song-volume": (data) => {
        songVolume = parseInt(data);
        applyVolume();
    },
    next: () => playNext().catch(console.error),
    prev: () => playPrev().catch(console.error),
    pause: () => backend?.pause(),
    play: () => backend?.play(),
});
