#!/bin/sh
# SPDX-License-Identifier: MIT
#
# Runs as root inside the Gitea container (issue #71) before Gitea starts:
# installs the server-side `git` that Gitea shells out to, behind a thin
# logging wrapper, and then hands over to the image's entrypoint.
#
#   m5-install-server-git.sh rgit|reference [entrypoint args...]
#
# rgit:      /rgit/rgit becomes the only git in the container. The image's
#            git and its exec path move to /opt/refgit (not on PATH); the
#            `refgit` command runs it, for the test's own verification.
# reference: the image's git stays, behind the same wrapper (the baseline
#            run, to tell rgit failures from Gitea's normal non-zero exits).
#
# The wrapper is /usr/bin/git (and the dashed git-* links to it). It runs
# the program of the same name in /usr/libexec/git-core and appends to
# /data/git-calls.log a `start` line per call (GIT_PROTOCOL, directory,
# arguments) and an `rc=<exit status>` line when it ends, followed by the
# stderr of failed calls. stdin and stdout pass through untouched; stderr
# is buffered per call.
set -eu

kind=$1
shift

# relink <dir>: the image's git-core links point to ../../bin/git; move the
# real binary into <dir> and point the links at it.
relink() {
    rm -f "$1/git"
    mv /usr/bin/git "$1/git"
    for f in "$1"/git-*; do
        if [ "$(readlink "$f" 2>/dev/null)" = ../../bin/git ]; then
            ln -sf git "$f"
        fi
    done
}

if [ "$kind" = rgit ]; then
    mkdir -p /opt/refgit
    mv /usr/libexec/git-core /opt/refgit/git-core
    relink /opt/refgit/git-core
    cat >/usr/local/bin/refgit <<'EOF'
#!/bin/sh
GIT_EXEC_PATH=/opt/refgit/git-core exec /opt/refgit/git-core/git "$@"
EOF
    mkdir -p /usr/libexec/git-core
    install -m 755 /rgit/rgit /usr/libexec/git-core/git
    for cmd in $(/usr/libexec/git-core/git --list-cmds=builtins); do
        ln -sf git "/usr/libexec/git-core/git-$cmd"
    done
else
    relink /usr/libexec/git-core
    cat >/usr/local/bin/refgit <<'EOF'
#!/bin/sh
exec /usr/libexec/git-core/git "$@"
EOF
fi
chmod 755 /usr/local/bin/refgit

# Dashed programs on PATH (git-upload-pack, git-receive-pack, git-shell, ...);
# those the server git lacks are removed. With the reference git they go
# through the wrapper. rgit's go straight to rgit: rgit hands requests it
# cannot serve yet (upload-pack v0) to a `git-<cmd>` on PATH that is not
# rgit itself, and a wrapper script around rgit would loop forever.
for prog in /usr/bin/git-*; do
    [ -e "$prog" ] || [ -L "$prog" ] || continue
    rm -f "$prog"
    if [ -e "/usr/libexec/git-core/${prog#/usr/bin/}" ]; then
        if [ "$kind" = rgit ]; then
            ln -s "/usr/libexec/git-core/${prog#/usr/bin/}" "$prog"
        else
            ln -s git "$prog"
        fi
    fi
done

cat >/usr/bin/git <<'EOF'
#!/bin/sh
log=/data/git-calls.log
name=${0##*/}
# Safety net against a program that calls itself through the wrapper.
depth=${M5_GIT_WRAPPER_DEPTH:-0}
if [ "$depth" -ge 40 ]; then
    echo "git wrapper: nested $depth deep, giving up" >&2
    exit 125
fi
export M5_GIT_WRAPPER_DEPTH=$((depth + 1))
# The arguments as [a] [b] ..., newlines shown as |.
args=$(
    printf '[%s]' "$name"
    for a; do
        case "$a" in
        (*'
'*) printf ' [%s]' "$(printf '%s' "$a" | tr '\n' '|')" ;;
        (*) printf ' [%s]' "$a" ;;
        esac
    done
)
# One write per line, so that concurrent calls do not interleave. The start
# line also records calls that never finish (Gitea kills its long-running
# `cat-file --batch` processes).
printf 'start uid=%s proto=%s pwd=%s argv: %s\n' "$(id -u)" "${GIT_PROTOCOL:-}" "$PWD" "$args" >>"$log"
err=$(mktemp)
"/usr/libexec/git-core/$name" "$@" 2>"$err"
rc=$?
cat "$err" >&2
entry=$(
    printf 'rc=%d uid=%s proto=%s pwd=%s argv: %s' "$rc" "$(id -u)" "${GIT_PROTOCOL:-}" "$PWD" "$args"
    if [ "$rc" -ne 0 ]; then printf '\n'; sed 's/^/    | /' "$err" | head -n 20; fi
)
printf '%s\n' "$entry" >>"$log"
rm -f "$err"
exit "$rc"
EOF
chmod 755 /usr/bin/git
touch /data/git-calls.log
chmod 666 /data/git-calls.log

exec /usr/bin/entrypoint "$@"
