#!/bin/sh
set -eu

container_browser_enabled="${BILI_SYNC_CONTAINER_BROWSER:-1}"
case "$(printf '%s' "$container_browser_enabled" | tr '[:upper:]' '[:lower:]')" in
    1|true|yes)
        export DISPLAY="${DISPLAY:-:99}"
        gui_home="/tmp/bili-sync-gui"
        mkdir -p "$gui_home"

        Xvfb "$DISPLAY" -screen 0 1280x900x24 -nolisten tcp -ac \
            >"/tmp/bili-sync-xvfb.log" 2>&1 &

        display_number="${DISPLAY#:}"
        display_number="${display_number%%.*}"
        display_socket="/tmp/.X11-unix/X${display_number}"
        attempts=0
        while [ ! -S "$display_socket" ] && [ "$attempts" -lt 50 ]; do
            attempts=$((attempts + 1))
            sleep 0.1
        done

        HOME="$gui_home" fluxbox -display "$DISPLAY" \
            >"/tmp/bili-sync-fluxbox.log" 2>&1 &
        x11vnc -display "$DISPLAY" -rfbport 5900 -localhost -forever -shared -nopw -noxdamage \
            >"/tmp/bili-sync-x11vnc.log" 2>&1 &
        ;;
esac

exec /app/bili-sync-rs "$@"
