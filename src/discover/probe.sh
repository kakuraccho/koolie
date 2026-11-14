# koolie host probe. Read-only: it runs `command -v`, `test`, `cat`, `awk`,
# `uname`, `id` and `herdr --version`, and nothing else. It never runs
# herdr-core or hc: almost every herdr-core subcommand starts a core.
#
# $1: herdr-core session (already trimmed and checked by koolie; default
# `default`).
#
# Output, between the BEGIN and END marker lines: one `key=value` per line.
# A list repeats its key. A value made of several fields separates them with a
# tab. Each field is percent-encoded for `%`, tab, CR and LF only.

tab=$(printf '\t')

enc() {
    printf '%s' "$1" | awk 'BEGIN { ORS = "" }
        { gsub(/%/, "%25"); gsub(/\t/, "%09"); gsub(/\r/, "%0D")
          if (NR > 1) print "%0A"
          print }'
}

kv() {
    kv_line="$1="
    shift
    kv_sep=
    for kv_v in "$@"; do
        kv_line="$kv_line$kv_sep$(enc "$kv_v")"
        kv_sep=$tab
    done
    printf '%s\n' "$kv_line"
}

# find_bin NAME FALLBACK...: the executable's path, from PATH, then the
# fallbacks (a non-interactive SSH exec has a short PATH).
find_bin() {
    fb_name=$1
    shift
    fb_p=$(command -v "$fb_name" 2>/dev/null) || fb_p=
    case $fb_p in
    /*)
        if [ -f "$fb_p" ] && [ -x "$fb_p" ]; then
            printf '%s' "$fb_p"
            return 0
        fi
        ;;
    esac
    for fb_c in "$@"; do
        if [ -f "$fb_c" ] && [ -x "$fb_c" ]; then
            printf '%s' "$fb_c"
            return 0
        fi
    done
    return 1
}

present() {
    if [ -S "$1" ]; then kv present "$1"; fi
}

# A rc file may have printed a line without its newline.
printf '\n%s\n' '@@KOOLIE-PROBE-BEGIN@@'
kv v 1

h=${HOME:-}
kv home "$h"
kv os "$(uname -s 2>/dev/null)"
kv arch "$(uname -m 2>/dev/null)"

# Herdr TUI.
hb=$(find_bin herdr "$h/.local/bin/herdr" /opt/homebrew/bin/herdr \
    /usr/local/bin/herdr "$h/.nix-profile/bin/herdr" \
    /run/current-system/sw/bin/herdr) || hb=
if [ -n "$hb" ]; then
    kv herdr_bin "$hb"
    if command -v timeout >/dev/null 2>&1; then
        hv=$(timeout 5 "$hb" --version </dev/null 2>/dev/null) || :
    else
        hv=$("$hb" --version </dev/null 2>/dev/null) || :
    fi
    kv herdr_version "$hv"
fi

hcfg=${XDG_CONFIG_HOME:-$h/.config}/herdr
if [ -S "$hcfg/herdr.sock" ]; then
    kv herdr_socket default "$hcfg/herdr.sock"
fi
for hs in "$hcfg"/sessions/*/herdr.sock; do
    [ -S "$hs" ] || continue
    hd=${hs%/herdr.sock}
    kv herdr_socket "${hd##*/}" "$hs"
done

# herdr-core: located, never run.
cb=$(find_bin herdr-core "$h/.local/bin/herdr-core" \
    /opt/homebrew/bin/herdr-core /usr/local/bin/herdr-core \
    "$h/.nix-profile/bin/herdr-core" \
    /run/current-system/sw/bin/herdr-core) || cb=
if [ -n "$cb" ]; then kv core_bin "$cb"; fi

cs=${1:-default}
kv core_session "$cs"
cst=${XDG_STATE_HOME:-$h/.local/state}/herdr-core/$cs
# A running core writes the socket path it bound into core.lock (no newline).
if [ -f "$cst/core.lock" ] && [ -r "$cst/core.lock" ]; then
    cl=$(cat "$cst/core.lock" 2>/dev/null) || cl=
    if [ -n "$cl" ]; then
        kv core_lock "$cl"
        present "$cl"
    fi
fi
# Where a core would listen: HC_SOCKET, else `socket` in config, else the
# runtime dir, else the state dir.
if [ -n "${HC_SOCKET:-}" ]; then
    cd_sock=$HC_SOCKET
    cd_src=env
else
    cconf=${XDG_CONFIG_HOME:-$h/.config}/herdr-core/config.toml
    cc_sock=
    if [ -f "$cconf" ] && [ -r "$cconf" ]; then
        cc_sock=$(awk '
            /^[ \t]*\[/ { exit }
            /^[ \t]*socket[ \t]*=/ {
                v = $0
                sub(/^[ \t]*socket[ \t]*=[ \t]*/, "", v)
                q = substr(v, 1, 1)
                if (q == "\"" || q == "\047") {
                    v = substr(v, 2)
                    i = index(v, q)
                    if (i > 0) v = substr(v, 1, i - 1)
                }
                print v
                exit
            }' "$cconf" 2>/dev/null) || cc_sock=
    fi
    if [ -n "$cc_sock" ]; then
        cd_sock=$cc_sock
        cd_src=config
    else
        crun=${XDG_RUNTIME_DIR:-}
        if [ -z "$crun" ]; then
            cu=$(id -u 2>/dev/null) || cu=
            if [ -n "$cu" ] && [ -d "/run/user/$cu" ]; then crun=/run/user/$cu; fi
        fi
        if [ -n "$crun" ]; then
            cd_sock=$crun/herdr-core/$cs.sock
        else
            cd_sock=$cst/$cs.sock
        fi
        cd_src=default
    fi
fi
kv core_default_socket "$cd_sock" "$cd_src"
present "$cd_sock"

# koolie itself (for a remote bridge, later).
kb=$(find_bin koolie "$h/.cargo/bin/koolie" "$h/.local/bin/koolie") || kb=
if [ -n "$kb" ]; then kv koolie_bin "$kb"; fi

printf '%s\n' '@@KOOLIE-PROBE-END@@'
