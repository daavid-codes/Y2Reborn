#!/usr/bin/env bash
# Reborn's Jellyfin client against a real Jellyfin server, no server needed:
# starts Jellyfin in Docker with a generated test library, sets it up (an
# administrator, the music library, Quick Connect is on by default) and runs
# crates/reborn-platform/tests/jellyfin_live.rs against it.
#
# Needs docker, ffmpeg (with ffprobe), curl, python3 and cargo.
#   tools/jellyfin-test/run.sh            # run, then remove the server
#   KEEP=1 tools/jellyfin-test/run.sh     # leave the server running
# JELLYFIN_IMAGE picks another version (default jellyfin/jellyfin:10.11.0).
set -euo pipefail

IMAGE=${JELLYFIN_IMAGE:-jellyfin/jellyfin:10.11.0}
PORT=8096
NAME=reborn-jellyfin-test
SERVER="http://127.0.0.1:$PORT"
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
WORK=$(mktemp -d)
# The server is local: no proxy may stand in between.
export NO_PROXY="127.0.0.1,localhost${NO_PROXY:+,$NO_PROXY}"
export no_proxy="$NO_PROXY"

for tool in docker ffmpeg ffprobe curl python3 cargo; do
    command -v "$tool" >/dev/null || { echo "missing: $tool" >&2; exit 2; }
done

cleanup() {
    if [ "${KEEP:-0}" = 1 ]; then
        echo "Jellyfin keeps running at $SERVER (user david, password test); remove it with: docker rm -f $NAME"
    else
        docker rm -f "$NAME" >/dev/null 2>&1 || true
        # The server's files belong to its own user: it removes them itself.
        docker run --rm -v "$WORK:/work" --entrypoint rm "$IMAGE" -rf /work/config /work/cache \
            >/dev/null 2>&1 || true
        rm -rf "$WORK" || true
    fi
}
trap cleanup EXIT

echo "== test library"
# Every song: 440 Hz for 10 s, then 1000 Hz for 10 s (the test listens
# for which tone arrives after a seek).
tones=(-f lavfi -i "sine=frequency=440:duration=10" -f lavfi -i "sine=frequency=1000:duration=10"
    -filter_complex "[0][1]concat=n=2:v=0:a=1,aformat=channel_layouts=stereo" -ar 44100)
album="$WORK/music/Test Artist/Tone Album"
other="$WORK/music/Other Artist/Other Album"
mkdir -p "$album" "$other" "$WORK/config" "$WORK/cache"
for n in 1 2 3; do
    ffmpeg -loglevel error "${tones[@]}" -c:a flac -metadata title="Tone $n" \
        -metadata artist="Test Artist" -metadata album_artist="Test Artist" \
        -metadata album="Tone Album" -metadata track=$n -metadata date=2026 \
        "$album/0$n Tone $n.flac"
done
tags=(-metadata artist="Other Artist" -metadata album_artist="Other Artist" -metadata album="Other Album")
ffmpeg -loglevel error "${tones[@]}" -c:a aac -b:a 192k -metadata title="Moov Song" "${tags[@]}" \
    -metadata track=1 "$other/01 Moov Song.m4a"
ffmpeg -loglevel error "${tones[@]}" -c:a libmp3lame -b:a 192k -metadata title="MP3 Song" "${tags[@]}" \
    -metadata track=2 "$other/02 MP3 Song.mp3"
# The server runs as another user in its container.
chmod -R a+rwX "$WORK"

echo "== Jellyfin ($IMAGE)"
docker rm -f "$NAME" >/dev/null 2>&1 || true
if [ "$(uname -s)" = Linux ]; then
    # Host networking: discovery's broadcast reaches it as on a home network.
    network=(--network host)
else
    network=(-p "$PORT:$PORT" -p 7359:7359/udp)
fi
docker run -d --name "$NAME" "${network[@]}" \
    -v "$WORK/config:/config" -v "$WORK/cache:/cache" -v "$WORK/music:/music:ro" \
    "$IMAGE" >/dev/null

api() { # method path [json body] [token]
    local auth="MediaBrowser Client=\"reborn-test\", Device=\"script\", DeviceId=\"reborn-test-setup\", Version=\"1\""
    [ -n "${4:-}" ] && auth="$auth, Token=\"$4\""
    curl -sS --fail -X "$1" "$SERVER$2" -H "Authorization: $auth" \
        -H "Content-Type: application/json" -H "Accept: application/json" ${3:+-d "$3"}
}

for _ in $(seq 1 90); do
    api GET /System/Info/Public 2>/dev/null | grep -q '"Id"' && break
    sleep 2
done
api GET /System/Info/Public >/dev/null || { docker logs "$NAME" | tail -50; exit 1; }

echo "== setup"
api POST /Startup/Configuration '{"UICulture":"en-US","MetadataCountryCode":"US","PreferredMetadataLanguage":"en"}' >/dev/null
api GET /Startup/User >/dev/null
api POST /Startup/User '{"Name":"david","Password":"test"}' >/dev/null
api POST /Startup/RemoteAccess '{"EnableRemoteAccess":true,"EnableAutomaticPortMapping":false}' >/dev/null
api POST /Startup/Complete >/dev/null
TOKEN=$(api POST /Users/AuthenticateByName '{"Username":"david","Pw":"test"}' |
    python3 -c 'import json,sys; print(json.load(sys.stdin)["AccessToken"])')
api POST "/Library/VirtualFolders?name=Music&collectionType=music&paths=/music&refreshLibrary=true" \
    '{"LibraryOptions":{}}' "$TOKEN" >/dev/null
songs=0
for _ in $(seq 1 60); do
    songs=$(api GET "/Items?Recursive=true&IncludeItemTypes=Audio" "" "$TOKEN" |
        python3 -c 'import json,sys; print(json.load(sys.stdin)["TotalRecordCount"])')
    [ "$songs" -ge 5 ] && break
    sleep 2
done
[ "$songs" -ge 5 ] || { echo "the library scan found $songs of 5 songs" >&2; exit 1; }
# Songs appear before the scan has filled in their albums: wait for the
# scan to finish and both albums to name their artist.
ready=0
for _ in $(seq 1 60); do
    scanning=$(api GET /ScheduledTasks "" "$TOKEN" |
        python3 -c 'import json,sys; print(any(t.get("Key")=="RefreshLibrary" and t.get("State")!="Idle" for t in json.load(sys.stdin)))')
    albums=$(api GET "/Items?Recursive=true&IncludeItemTypes=MusicAlbum" "" "$TOKEN" |
        python3 -c 'import json,sys; print(sum(1 for i in json.load(sys.stdin)["Items"] if i.get("AlbumArtist")))')
    if [ "$scanning" = False ] && [ "$albums" -ge 2 ]; then ready=1; break; fi
    sleep 2
done
[ "$ready" = 1 ] || { echo "the library scan didn't finish" >&2; exit 1; }
echo "$songs songs scanned"

echo "== Reborn's client"
cd "$ROOT"
REBORN_JELLYFIN_TEST_SERVER="$SERVER" REBORN_JELLYFIN_TEST_TOKEN="$TOKEN" \
    cargo test -p reborn-platform --locked --offline --test jellyfin_live -- --ignored --nocapture
