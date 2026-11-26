#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
#
# M5 end-to-end test (issue #71): Gitea with rgit as its server-side git.
#
# A pinned Gitea container gets rgit installed as the only `git`
# (m5-install-server-git.sh: /usr/bin/git, the dashed programs and the exec
# path). Gitea then shells out to rgit for everything: `git version` at
# start-up, init and the auto-init commit, upload-pack and receive-pack over
# HTTPS and SSH, its hooks, the web views (log, diff, blame, compare),
# archives, file edits through the API, pull request checks and merges,
# tags and releases, wiki, forks, mirrors and maintenance.
#
# The scenario runs once per client (the reference git and rgit on the host,
# each alone on PATH). Each step is reported PASS or FAIL; a FAIL listed in
# m5-gitea-server.known (with its issue) is expected and does not fail the
# run, any other FAIL does. Every git call Gitea makes is logged inside the
# container; the calls that failed are summarised at the end (some non-zero
# exits are normal Gitea probing; `--server reference` shows the baseline).
#
# Usage: scripts/acceptance/m5-gitea-server.sh [options]
#   --rgit <path>        rgit binary; static (musl) when --server rgit, since
#                        it runs inside the Alpine image (default:
#                        target/x86_64-unknown-linux-musl/release/rgit)
#   --server rgit|reference
#                        server-side git (default rgit); `reference` keeps the
#                        image's git, to check the scenario itself
#   --clients "<list>"   subset of: git rgit (default both)
#   --keep               keep the container and the work directory
#   --inventory <file>   write the distinct forms of Gitea's git calls
#                        (command and options; arguments masked) to <file>
#
# Needs on the host: docker, curl, jq, openssl, ssh, ssh-keygen, ssh-keyscan,
# unzip, tar, timeout,
# and git for the reference client. The container's ports are bound to
# 127.0.0.1 only.
set -euo pipefail
shopt -s inherit_errexit

GITEA_IMAGE=${GITEA_IMAGE:-gitea/gitea:1.24.6}
USER_NAME=rgit-ci
USER_PASS="accept-$RANDOM-$RANDOM-pw"

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(cd "$here/../.." && pwd)
rgit=$root/target/x86_64-unknown-linux-musl/release/rgit
server=rgit
clients="git rgit"
keep=0
inventory=""
while [ $# -gt 0 ]; do
    case "$1" in
    --rgit) rgit=$2; shift 2 ;;
    --inventory) inventory=$(cd "$(dirname "$2")" && pwd)/$(basename "$2"); shift 2 ;;
    --server) server=$2; shift 2 ;;
    --clients) clients=$2; shift 2 ;;
    --keep) keep=1; shift ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done
case "$server" in rgit | reference) ;; *) echo "--server: rgit or reference" >&2; exit 2 ;; esac
rgit=$(cd "$(dirname "$rgit")" && pwd)/$(basename "$rgit")
[ -x "$rgit" ] || { echo "rgit not found: $rgit" >&2; exit 2; }
refgit=$(command -v git) || { echo "no reference git on the host" >&2; exit 2; }

work=$(mktemp -d "${TMPDIR:-/tmp}/rgit-m5.XXXXXX")
container=rgit-m5-$(date +%s)-$$-$RANDOM
cleanup() {
    local status=$?
    if [ "$keep" = 1 ]; then
        echo "kept: container $container, work directory $work"
    else
        docker rm -f -v "$container" >/dev/null 2>&1 || true
        rm -rf "$work"
    fi
    exit "$status"
}
trap cleanup EXIT

log() { printf '\n=== %s\n' "$*"; }
die() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

# --- TLS: a throwaway CA and a server certificate for 127.0.0.1 -------------
mkdir -p "$work/certs"
(
    cd "$work/certs"
    openssl req -x509 -newkey rsa:2048 -nodes -days 2 -keyout ca.key -out ca.pem \
        -subj "/CN=rgit acceptance CA" \
        -addext "basicConstraints=critical,CA:TRUE" \
        -addext "keyUsage=critical,keyCertSign,cRLSign" 2>/dev/null
    openssl req -newkey rsa:2048 -nodes -keyout key.pem -out server.csr \
        -subj "/CN=127.0.0.1" 2>/dev/null
    printf '%s\n' "basicConstraints=CA:FALSE" "keyUsage=critical,digitalSignature,keyEncipherment" \
        "extendedKeyUsage=serverAuth" "subjectAltName=IP:127.0.0.1,DNS:localhost" >ext.cnf
    openssl x509 -req -in server.csr -CA ca.pem -CAkey ca.key -CAcreateserial -days 2 \
        -extfile ext.cnf -out cert.pem 2>/dev/null
    chmod 644 key.pem cert.pem
)

# --- Gitea with the server-side git ------------------------------------------
log "gitea ($GITEA_IMAGE) as $container, server git: $server"
docker create --name "$container" \
    -p 127.0.0.1::3000 -p 127.0.0.1::22 -p 127.0.0.1::2222 \
    --entrypoint /bin/sh \
    -e GITEA__security__INSTALL_LOCK=true \
    -e GITEA__security__DISABLE_GIT_HOOKS=false \
    -e GITEA__database__DB_TYPE=sqlite3 \
    -e GITEA__server__PROTOCOL=https \
    -e GITEA__server__CERT_FILE=/certs/cert.pem \
    -e GITEA__server__KEY_FILE=/certs/key.pem \
    -e GITEA__server__ROOT_URL=https://127.0.0.1:3000/ \
    -e GITEA__server__LFS_START_SERVER=false \
    -e GITEA__server__START_SSH_SERVER=true \
    -e GITEA__server__SSH_LISTEN_PORT=2222 \
    -e GITEA__server__SSH_CREATE_AUTHORIZED_KEYS_FILE=true \
    -e GITEA__service__DISABLE_REGISTRATION=true \
    -e GITEA__repository__ENABLE_PUSH_CREATE_USER=true \
    -e GITEA__migrations__ALLOW_LOCALNETWORKS=true \
    -e GITEA__migrations__SKIP_TLS_VERIFY=true \
    -e GITEA__log__LEVEL=Info \
    "$GITEA_IMAGE" /install-server-git.sh "$server" /usr/bin/s6-svscan /etc/s6 >/dev/null
docker cp "$work/certs/." "$container:/certs"
docker cp "$here/m5-install-server-git.sh" "$container:/install-server-git.sh"
if [ "$server" = rgit ]; then
    mkdir -p "$work/rgit-bin"
    cp "$rgit" "$work/rgit-bin/rgit"
    docker cp "$work/rgit-bin/." "$container:/rgit"
fi
docker start "$container" >/dev/null
http_port=$(docker port "$container" 3000/tcp | head -n 1 | sed 's/.*://')
ssh_port=$(docker port "$container" 22/tcp | head -n 1 | sed 's/.*://')
builtin_ssh_port=$(docker port "$container" 2222/tcp | head -n 1 | sed 's/.*://')
base=https://127.0.0.1:$http_port
api=$base/api/v1
echo "https port $http_port, ssh port $ssh_port (OpenSSH), $builtin_ssh_port (Gitea built-in)"

for _ in $(seq 120); do
    curl -fsS --cacert "$work/certs/ca.pem" "$base/api/healthz" >/dev/null 2>&1 && break
    sleep 1
done
curl -fsS --cacert "$work/certs/ca.pem" "$base/api/healthz" >/dev/null ||
    {
        docker logs "$container" 2>&1 | tail -n 50
        docker exec "$container" sh -c 'cat /data/git-calls.log; ps -ef' || true
        die "gitea did not come up"
    }
docker exec "$container" git version

gitea_exec() { docker exec -u git "$container" "$@"; }
# curl_api <curl args...>: JSON API call with the token; fails on HTTP >= 400.
curl_api() {
    curl -fsS --cacert "$work/certs/ca.pem" -H 'Content-Type: application/json' \
        -H "Authorization: token $token" "$@"
}
# http_get <path> <out>: an authenticated GET; prints the status code.
http_get() {
    curl -sS -o "$2" -w '%{http_code}' --cacert "$work/certs/ca.pem" \
        -u "$USER_NAME:$token" "$base$1"
}

gitea_exec gitea admin user create --username "$USER_NAME" --password "$USER_PASS" \
    --email rgit-ci@example.com --admin --must-change-password=false >/dev/null
token=$(curl -fsS --cacert "$work/certs/ca.pem" -H 'Content-Type: application/json' \
    -u "$USER_NAME:$USER_PASS" -X POST "$api/users/$USER_NAME/tokens" \
    -d '{"name":"acceptance","scopes":["all"]}' | jq -r .sha1)
[ -n "$token" ] && [ "$token" != null ] || die "no access token"
ssh-keygen -q -t ed25519 -N '' -C rgit-acceptance -f "$work/id_ed25519"
key_id=$(curl_api -X POST "$api/user/keys" \
    -d "$(jq -n --arg k "$(cat "$work/id_ed25519.pub")" '{title:"acceptance",key:$k}')" | jq -r .id)
# With its built-in SSH server running, Gitea does not write the OpenSSH
# authorized_keys file; write the entry Gitea would write.
docker exec -u git -e KEY="$(cat "$work/id_ed25519.pub")" -e KEY_ID="$key_id" "$container" sh -ec '
    mkdir -p /data/git/.ssh
    printf "command=\"/usr/local/bin/gitea --config=/data/gitea/conf/app.ini serv key-%s\",no-port-forwarding,no-X11-forwarding,no-agent-forwarding,no-pty,no-user-rc,restrict %s\n" \
        "$KEY_ID" "$KEY" >/data/git/.ssh/authorized_keys
    chmod 600 /data/git/.ssh/authorized_keys'
host_key=$(docker exec "$container" cat /data/ssh/ssh_host_ed25519_key.pub | cut -d' ' -f1,2)
printf '[127.0.0.1]:%s %s\n' "$ssh_port" "$host_key" >"$work/known_hosts"
# Gitea's built-in SSH server makes its own host key on start-up.
ssh-keyscan -p "$builtin_ssh_port" 127.0.0.1 2>/dev/null >>"$work/known_hosts" ||
    die "no host key from Gitea's built-in SSH server"

# --- client sandboxes: the client under test is the only `git` --------------
for c in $clients; do
    sb=$work/bin-$c
    mkdir -p "$sb" "$work/home-$c"
    case "$c" in
    git) ln -s "$refgit" "$sb/git" ;;
    rgit) ln -s "$rgit" "$sb/git" ;;
    *) die "unknown client $c" ;;
    esac
    for tool in sh bash cat cut sed grep mv rm mkdir chmod ssh printf head tail tr wc; do
        path=$(command -v "$tool") || die "$tool not found on the host"
        ln -s "$path" "$sb/$tool"
    done
    printf '[user]\n\tname = Acceptance Tester\n\temail = tester@example.com\n[init]\n\tdefaultBranch = main\n' \
        >"$work/home-$c/.gitconfig"
done

# cg <client> <git args...>: run the client git in its sandbox.
cg() {
    local c=$1
    shift
    # Never touch a repository outside the work directory (a failed `cd`
    # in a step must not reach the checkout the test runs from).
    case "$PWD" in
    "$work" | "$work"/*) ;;
    *) echo "cg: refusing to run in $PWD" >&2; return 1 ;;
    esac
    timeout 300 env -i PATH="$work/bin-$c" HOME="$work/home-$c" XDG_CONFIG_HOME="$work/home-$c/.config" \
        GIT_CONFIG_NOSYSTEM=1 LC_ALL=C TZ=UTC GIT_TERMINAL_PROMPT=0 \
        GIT_SSL_CAINFO="$work/certs/ca.pem" GIT_CEILING_DIRECTORIES="$work" \
        GIT_SSH_COMMAND="ssh -F /dev/null -i $work/id_ed25519 -o IdentitiesOnly=yes -o BatchMode=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile=$work/known_hosts" \
        git "$@"
}

https_url() { echo "https://$USER_NAME:$token@127.0.0.1:$http_port/$USER_NAME/$1.git"; }
ssh_url() { echo "ssh://git@127.0.0.1:$ssh_port/$USER_NAME/$1.git"; }
builtin_ssh_url() { echo "ssh://git@127.0.0.1:$builtin_ssh_port/$USER_NAME/$1.git"; }
repo_dir() { echo "/data/git/repositories/$USER_NAME/$1.git"; }

# --- step runner ----------------------------------------------------------------
known_file=$here/m5-gitea-server.known
mkdir -p "$work/steps"
passed=() failed=() expected=() fixed=()
# is_known <step>: listed in the known-failures file (rgit server only).
is_known() {
    [ "$server" = rgit ] || return 1
    grep -v '^#' "$known_file" 2>/dev/null | awk '{print $1}' | grep -qxF "$1"
}
# step <name> <function> [args...]: run one scenario step and record it.
step() {
    local name=$1
    shift
    local out=$work/steps/$name.log rc=0
    # Mark the step in the log of Gitea's git calls.
    docker exec "$container" sh -c "echo '### step $name' >>/data/git-calls.log"
    # Not in a condition: errexit must stay active inside the step.
    set +e
    (
        set -e
        cd "$work"
        "$@"
    ) >"$out" 2>&1
    rc=$?
    set -e
    if [ "$rc" = 0 ]; then
        if is_known "$name"; then
            fixed+=("$name")
            printf 'PASS  %s (listed as known failure; remove it from the list)\n' "$name"
        else
            passed+=("$name")
            printf 'PASS  %s\n' "$name"
        fi
    else
        if is_known "$name"; then
            expected+=("$name")
            printf 'XFAIL %s\n' "$name"
        else
            failed+=("$name")
            printf 'FAIL  %s\n' "$name"
        fi
        sed 's/^/      | /' "$out" | tail -n 25
    fi
}
fail() { echo "$*" >&2; return 1; }

# wait_for <seconds> <command...>: retry until the command succeeds.
wait_for() {
    local n=$1
    shift
    for _ in $(seq "$n"); do
        "$@" && return 0
        sleep 1
    done
    "$@"
}

# --- scenario steps -------------------------------------------------------------
# All steps of client $C work on repository $R (and $R-*).

s_create_repo() {
    curl_api -X POST "$api/user/repos" -d "$(jq -n --arg n "$R" \
        '{name:$n, auto_init:true, readme:"Default", gitignores:"Rust", license:"MIT", default_branch:"main"}')" >/dev/null
    local info
    info=$(curl_api "$api/repos/$USER_NAME/$R")
    [ "$(jq -r .empty <<<"$info")" = false ] || fail "repository is empty after auto-init: $info"
    curl_api "$api/repos/$USER_NAME/$R/contents/LICENSE" | jq -e '.type == "file"' >/dev/null
}

s_clone_https() {
    cg "$C" clone "$(https_url "$R")" "$W/https"
    [ -f "$W/https/README.md" ] && [ -f "$W/https/LICENSE" ] && [ -f "$W/https/.gitignore" ]
}

s_push_https() {
    cd "$W/https"
    mkdir -p src
    printf 'alpha\nbeta\ngamma\ndelta\nepsilon\n' >src/lib.txt
    printf '#!/bin/sh\necho tool\n' >tool.sh
    chmod +x tool.sh
    printf 'fn main() {\n    println!("hello");\n}\n' >src/main.rs
    cg "$C" add .
    cg "$C" commit -q -m "add sources"
    printf 'zeta\n' >>src/lib.txt
    cg "$C" commit -q -a -m "extend lib"
    cg "$C" push origin main
    local remote
    remote=$(curl_api "$api/repos/$USER_NAME/$R/branches/main" | jq -r .commit.id)
    [ "$remote" = "$(cg "$C" rev-parse HEAD)" ] || fail "main on the server is $remote"
}

s_push_ssh() {
    cd "$W/https"
    cg "$C" remote add ssh "$(ssh_url "$R")"
    cg "$C" checkout -q -b feature
    printf 'feature work\n' >feature.txt
    cg "$C" add feature.txt
    cg "$C" commit -q -m "feature work"
    cg "$C" push ssh feature
    cg "$C" tag -a -m "version one" v1.0 main
    cg "$C" push ssh v1.0
    cg "$C" checkout -q main
    curl_api "$api/repos/$USER_NAME/$R/branches/feature" | jq -e --arg id "$(cg "$C" rev-parse feature)" \
        '.commit.id == $id' >/dev/null
    curl_api "$api/repos/$USER_NAME/$R/tags/v1.0" | jq -e '.message | startswith("version one")' >/dev/null
}

s_clone_ssh() {
    cg "$C" clone "$(ssh_url "$R")" "$W/ssh"
    [ "$(cg "$C" -C "$W/ssh" rev-parse HEAD)" = "$(cg "$C" -C "$W/https" rev-parse main)" ]
    cg "$C" -C "$W/ssh" rev-parse --verify -q refs/remotes/origin/feature >/dev/null
}

# last_upload_pack_proto: GIT_PROTOCOL of Gitea's latest upload-pack call.
last_upload_pack_proto() {
    docker exec "$container" cat /data/git-calls.log | grep 'upload-pack\]' | tail -n 1 |
        sed -n 's/.* proto=\([^ ]*\) .*/\1/p'
}

# Gitea's own SSH server (Go), next to the image's OpenSSH.
s_builtin_ssh() {
    cg "$C" clone "$(builtin_ssh_url "$R")" "$W/builtin"
    echo "upload-pack GIT_PROTOCOL: '$(last_upload_pack_proto)'"
    cd "$W/builtin"
    [ "$(cg "$C" rev-parse HEAD)" = "$(cg "$C" -C "$W/https" rev-parse main)" ]
    cg "$C" checkout -q -b builtin-ssh
    printf 'over the built-in server\n' >builtin.txt
    cg "$C" add builtin.txt
    cg "$C" commit -q -m "push over the built-in SSH server"
    cg "$C" push origin builtin-ssh
    cg "$C" fetch origin
    curl_api "$api/repos/$USER_NAME/$R/branches/builtin-ssh" | jq -e --arg id "$(cg "$C" rev-parse HEAD)" \
        '.commit.id == $id' >/dev/null
}

# Protocol v0 (no GIT_PROTOCOL): HTTPS and both SSH servers.
s_protocol_v0_https() {
    cg "$C" -c protocol.version=0 clone "$(https_url "$R")" "$W/v0-https"
    cg "$C" -C "$W/v0-https" -c protocol.version=0 fetch origin
    cg "$C" -C "$W/v0-https" -c protocol.version=0 ls-remote origin >/dev/null
}

s_protocol_v0_ssh() {
    cg "$C" -c protocol.version=0 clone "$(ssh_url "$R")" "$W/v0-ssh"
    cg "$C" -c protocol.version=0 clone "$(builtin_ssh_url "$R")" "$W/v0-builtin"
    cg "$C" -C "$W/v0-ssh" -c protocol.version=1 fetch origin
}

s_api_files() {
    local sha
    curl_api -X POST "$api/repos/$USER_NAME/$R/contents/docs/new.md" -d "$(jq -n \
        --arg c "$(printf '# new\n\ncreated through the API\n' | base64 -w0)" \
        '{content:$c, message:"api: create docs/new.md", branch:"main"}')" >/dev/null
    sha=$(curl_api "$api/repos/$USER_NAME/$R/contents/src/lib.txt?ref=main" | jq -r .sha)
    curl_api -X PUT "$api/repos/$USER_NAME/$R/contents/src/lib.txt" -d "$(jq -n --arg s "$sha" \
        --arg c "$(printf 'alpha\nbeta\nGAMMA\ndelta\nepsilon\nzeta\n' | base64 -w0)" \
        '{content:$c, sha:$s, message:"api: edit lib", branch:"main"}')" >/dev/null
    sha=$(curl_api "$api/repos/$USER_NAME/$R/contents/tool.sh?ref=main" | jq -r .sha)
    curl_api -X DELETE "$api/repos/$USER_NAME/$R/contents/tool.sh" -d "$(jq -n --arg s "$sha" \
        '{sha:$s, message:"api: delete tool.sh", branch:"main"}')" >/dev/null
    # Several files in one commit (ChangeFiles).
    curl_api -X POST "$api/repos/$USER_NAME/$R/contents" -d "$(jq -n \
        --arg a "$(printf 'one\n' | base64 -w0)" --arg b "$(printf 'two\n' | base64 -w0)" \
        '{files:[{operation:"create",path:"multi/a.txt",content:$a},{operation:"create",path:"multi/b.txt",content:$b}], message:"api: two files", branch:"main"}')" >/dev/null
    curl_api "$api/repos/$USER_NAME/$R/raw/src/lib.txt?ref=main" | grep -qx GAMMA
}

s_fetch() {
    cd "$W/https"
    cg "$C" fetch origin
    cg "$C" fetch ssh
    cg "$C" merge -q --ff-only origin/main
    grep -qx GAMMA src/lib.txt && [ ! -e tool.sh ] && [ -f multi/b.txt ] && [ -f docs/new.md ]
    cd "$W/ssh"
    cg "$C" pull -q --ff-only
    [ "$(cg "$C" rev-parse HEAD)" = "$(cg "$C" -C "$W/https" rev-parse HEAD)" ]
}

s_api_views() {
    local head first
    head=$(cg "$C" -C "$W/https" rev-parse HEAD)
    first=$(cg "$C" -C "$W/https" rev-list --max-parents=0 HEAD)
    curl_api "$api/repos/$USER_NAME/$R/commits?sha=main&limit=50" | jq -e 'length >= 7' >/dev/null
    curl_api "$api/repos/$USER_NAME/$R/commits?sha=main&path=src/lib.txt" | jq -e 'length == 3' >/dev/null
    curl_api "$api/repos/$USER_NAME/$R/git/commits/$head" | jq -e --arg h "$head" '.sha == $h' >/dev/null
    curl_api "$api/repos/$USER_NAME/$R/git/commits/$head.diff" | grep -q '^diff --git'
    curl_api "$api/repos/$USER_NAME/$R/git/commits/$head.patch" | grep -q '^Subject: '
    curl_api "$api/repos/$USER_NAME/$R/git/trees/main?recursive=true" | jq -e '.tree | map(.path) | index("src/main.rs")' >/dev/null
    curl_api "$api/repos/$USER_NAME/$R/compare/$first...main" | jq -e '.total_commits >= 6' >/dev/null
    curl_api "$api/repos/$USER_NAME/$R/branches" | jq -e 'length == 3' >/dev/null
    curl_api "$api/repos/$USER_NAME/$R/languages" >/dev/null
    curl_api "$api/repos/$USER_NAME/$R/contents?ref=main" | jq -e 'map(.name) | index("README.md")' >/dev/null
}

s_web_views() {
    local head code page
    head=$(cg "$C" -C "$W/https" rev-parse HEAD)
    for page in "" /src/branch/main /src/branch/main/src/lib.txt /src/commit/$head/src \
        /commits/branch/main /commits/branch/main/src/lib.txt "/commit/$head" \
        "/commit/$head.diff" "/commit/$head.patch" /blame/branch/main/src/lib.txt \
        /compare/main...feature /raw/branch/main/README.md /graph /branches /tags \
        /releases /activity /find/branch/main "/src/branch/main/src/main.rs?display=source"; do
        code=$(http_get "/$USER_NAME/$R$page" "$work/page.html")
        [ "$code" = 200 ] || fail "GET $page: HTTP $code"
        echo "GET $page: 200"
    done
    http_get "/$USER_NAME/$R/blame/branch/main/src/lib.txt" "$work/page.html" >/dev/null
    grep -q 'api: edit lib' "$work/page.html" || fail "blame does not show the API edit"
    http_get "/$USER_NAME/$R/commit/$head.diff" "$work/page.html" >/dev/null
    grep -q '^diff --git' "$work/page.html" || fail "commit .diff is not a diff"
}

s_archives() {
    local code
    code=$(http_get "/api/v1/repos/$USER_NAME/$R/archive/main.zip" "$W/a.zip")
    [ "$code" = 200 ] || fail "zip: HTTP $code"
    unzip -l "$W/a.zip" | grep -q "$R/src/lib.txt" || fail "zip lacks src/lib.txt"
    code=$(http_get "/api/v1/repos/$USER_NAME/$R/archive/main.tar.gz" "$W/a.tgz")
    [ "$code" = 200 ] || fail "tar.gz: HTTP $code"
    tar tzf "$W/a.tgz" | grep -q "$R/src/lib.txt" || fail "tar.gz lacks src/lib.txt"
    code=$(http_get "/$USER_NAME/$R/archive/v1.0.tar.gz" "$W/t.tgz")
    [ "$code" = 200 ] || fail "tag tar.gz: HTTP $code"
    code=$(http_get "/$USER_NAME/$R/archive/main.bundle" "$W/a.bundle")
    [ "$code" = 200 ] || fail "bundle: HTTP $code"
    cg "$C" -C "$W/https" bundle verify "$W/a.bundle" >/dev/null ||
        fail "bundle does not verify"
}

s_tags_releases() {
    curl_api -X POST "$api/repos/$USER_NAME/$R/tags" -d '{"tag_name":"v1.1","target":"main","message":"api tag"}' >/dev/null
    curl_api -X POST "$api/repos/$USER_NAME/$R/tags" -d '{"tag_name":"light","target":"main"}' >/dev/null
    curl_api -X POST "$api/repos/$USER_NAME/$R/releases" -d '{"tag_name":"v1.0","name":"One","body":"first"}' >/dev/null
    curl_api -X POST "$api/repos/$USER_NAME/$R/releases" \
        -d '{"tag_name":"v2.0","target_commitish":"main","name":"Two","body":"new tag"}' >/dev/null
    curl_api "$api/repos/$USER_NAME/$R/tags" | jq -e 'map(.name) | sort == ["light","v1.0","v1.1","v2.0"]' >/dev/null
    curl_api -X DELETE "$api/repos/$USER_NAME/$R/tags/light" >/dev/null
    cg "$C" -C "$W/https" fetch -q --tags origin
    cg "$C" -C "$W/https" rev-parse -q --verify refs/tags/v2.0 >/dev/null
    [ "$(cg "$C" -C "$W/https" cat-file -t v1.1)" = tag ] || fail "v1.1 is not an annotated tag"
}

s_branches_api() {
    curl_api -X POST "$api/repos/$USER_NAME/$R/branches" -d '{"new_branch_name":"tmp","old_ref_name":"main"}' >/dev/null
    curl_api -X PATCH "$api/repos/$USER_NAME/$R/branches/tmp" -d '{"name":"tmp2"}' >/dev/null
    curl_api "$api/repos/$USER_NAME/$R/branches/tmp2" >/dev/null
    curl_api -X DELETE "$api/repos/$USER_NAME/$R/branches/tmp2" >/dev/null
    ! curl_api "$api/repos/$USER_NAME/$R/branches/tmp2" >/dev/null 2>&1
}

# new_pr_branch <branch> <file> <content>: a branch off the server's main
# with one commit, pushed over HTTPS; prints the PR number.
new_pr_branch() {
    cd "$W/https"
    cg "$C" fetch -q origin >&2
    cg "$C" checkout -q -B "$1" origin/main >&2
    printf '%s\n' "$3" >"$2"
    cg "$C" add "$2" >&2
    cg "$C" commit -q -m "change $2 on $1" >&2
    printf 'second change\n' >>"$2"
    cg "$C" commit -q -a -m "second change of $2 on $1" >&2
    cg "$C" push -q origin "$1" >&2
    cg "$C" checkout -q main >&2
    curl_api -X POST "$api/repos/$USER_NAME/$R/pulls" -d "$(jq -n --arg b "$1" \
        '{head:$b, base:"main", title:("PR " + $b), body:"from the acceptance test"}')" | jq -r .number
}

pr_mergeable() { curl_api "$api/repos/$USER_NAME/$R/pulls/$1" | jq -e '.mergeable == true' >/dev/null; }

# s_pr <style>: open a pull request, check its views, merge with <style>.
s_pr() {
    local style=$1 n
    n=$(new_pr_branch "pr-$style" "pr-$style.txt" "merged with $style")
    [ -n "$n" ] && [ "$n" != null ] || fail "no pull request"
    wait_for 30 pr_mergeable "$n" || fail "PR $n never became mergeable"
    curl_api "$api/repos/$USER_NAME/$R/pulls/$n.diff" | grep -q "^diff --git a/pr-$style.txt"
    curl_api "$api/repos/$USER_NAME/$R/pulls/$n/files" | jq -e 'length == 1' >/dev/null
    curl_api "$api/repos/$USER_NAME/$R/pulls/$n/commits" | jq -e 'length == 2' >/dev/null
    [ "$(http_get "/$USER_NAME/$R/pulls/$n/files" "$work/page.html")" = 200 ] || fail "PR files page"
    # 405 while Gitea still re-checks the PR after the previous merge.
    local code
    for _ in $(seq 10); do
        code=$(curl -sS -o "$work/merge.out" -w '%{http_code}' --cacert "$work/certs/ca.pem" \
            -H "Authorization: token $token" -H 'Content-Type: application/json' \
            -X POST "$api/repos/$USER_NAME/$R/pulls/$n/merge" \
            -d "$(jq -n --arg s "$style" '{Do:$s, delete_branch_after_merge:true}')")
        [ "$code" = 405 ] || break
        echo "merge ($style): HTTP 405 $(cat "$work/merge.out"), retrying"
        sleep 2
    done
    [ "$code" = 200 ] || fail "merge ($style): HTTP $code $(cat "$work/merge.out")"
    curl_api "$api/repos/$USER_NAME/$R/pulls/$n" | jq -e '.merged == true' >/dev/null || fail "PR $n not merged"
    curl_api "$api/repos/$USER_NAME/$R/raw/pr-$style.txt?ref=main" | grep -q "merged with $style"
    local parents
    parents=$(curl_api "$api/repos/$USER_NAME/$R/git/commits/main" | jq '.parents | length')
    case "$style" in
    merge | rebase-merge) [ "$parents" = 2 ] || fail "$style: head of main has $parents parents" ;;
    *) [ "$parents" = 1 ] || fail "$style: head of main has $parents parents" ;;
    esac
}

s_pr_conflict() {
    local n code base
    cd "$W/https"
    cg "$C" fetch -q origin
    # Branch off an old main and change the line the API edit changed.
    base=$(cg "$C" log -1 --format=%H --grep='api: edit lib' origin/main)
    cg "$C" checkout -q -B conflict "$base^"
    printf 'alpha\nbeta\nCONFLICT\ndelta\nepsilon\nzeta\n' >src/lib.txt
    cg "$C" commit -q -a -m "conflicting change"
    cg "$C" push -q origin conflict
    cg "$C" checkout -q main
    n=$(curl_api -X POST "$api/repos/$USER_NAME/$R/pulls" \
        -d '{"head":"conflict","base":"main","title":"conflict"}' | jq -r .number)
    sleep 5
    for _ in $(seq 30); do
        curl_api "$api/repos/$USER_NAME/$R/pulls/$n" | jq -e '.mergeable == false' >/dev/null || fail "conflict PR is mergeable"
        code=$(curl -sS -o "$work/merge.out" -w '%{http_code}' --cacert "$work/certs/ca.pem" \
            -H "Authorization: token $token" -H 'Content-Type: application/json' \
            -X POST "$api/repos/$USER_NAME/$R/pulls/$n/merge" -d '{"Do":"merge"}')
        cat "$work/merge.out"
        echo " (HTTP $code)"
        [ "$code" = 405 ] || [ "$code" = 409 ] || fail "merge of a conflicting PR answered $code"
        grep -qi 'conflict\|not mergeable\|try again' "$work/merge.out"
        grep -qi 'try again' "$work/merge.out" || break
        sleep 1
    done
    curl_api "$api/repos/$USER_NAME/$R/pulls/$n" | jq -e '.merged == false' >/dev/null
    curl_api -X PATCH "$api/repos/$USER_NAME/$R/pulls/$n" -d '{"state":"closed"}' >/dev/null
}

s_agit() {
    local before after
    before=$(curl_api "$api/repos/$USER_NAME/$R/pulls?state=all&limit=50" | jq length)
    cd "$W/https"
    cg "$C" fetch -q origin
    cg "$C" checkout -q -B agit origin/main
    printf 'agit\n' >agit.txt
    cg "$C" add agit.txt
    cg "$C" commit -q -m "agit change"
    cg "$C" push origin HEAD:refs/for/main/agit-topic -o title="AGit PR" -o description="via refs/for"
    cg "$C" checkout -q main
    after=$(curl_api "$api/repos/$USER_NAME/$R/pulls?state=all&limit=50" | jq length)
    [ "$after" = $((before + 1)) ] || fail "AGit push created no pull request ($before -> $after)"
}

s_protected_branch() {
    curl_api -X POST "$api/repos/$USER_NAME/$R/branch_protections" \
        -d '{"rule_name":"main","enable_push":false}' >/dev/null
    cd "$W/https"
    cg "$C" fetch -q origin
    cg "$C" checkout -q -B main origin/main
    printf 'protected\n' >protected.txt
    cg "$C" add protected.txt
    cg "$C" commit -q -m "push to a protected branch"
    if cg "$C" push origin main 2>"$work/push.err"; then
        cat "$work/push.err"
        fail "push to a protected branch succeeded"
    fi
    cat "$work/push.err"
    grep -qi 'protected' "$work/push.err" || fail "no protected-branch message"
    cg "$C" reset -q --hard origin/main
    curl_api -X DELETE "$api/repos/$USER_NAME/$R/branch_protections/main" >/dev/null
}

s_git_hook() {
    local hook
    hook=$(printf '#!/bin/sh\nwhile read old new ref; do\n  case "$ref" in\n  refs/heads/reject-*) echo "custom hook rejects $ref"; exit 1 ;;\n  esac\ndone\nexit 0\n')
    curl_api -X PATCH "$api/repos/$USER_NAME/$R/hooks/git/pre-receive" \
        -d "$(jq -n --arg c "$hook" '{content:$c}')" >/dev/null
    cd "$W/https"
    if cg "$C" push origin main:refs/heads/reject-me 2>"$work/push.err"; then
        cat "$work/push.err"
        fail "the pre-receive hook did not reject the push"
    fi
    cat "$work/push.err"
    grep -q 'custom hook rejects refs/heads/reject-me' "$work/push.err" || fail "hook output not relayed"
    cg "$C" push origin main:refs/heads/accepted
    curl_api -X PATCH "$api/repos/$USER_NAME/$R/hooks/git/pre-receive" -d '{"content":""}' >/dev/null
}

s_wiki() {
    curl_api -X POST "$api/repos/$USER_NAME/$R/wiki/new" -d "$(jq -n \
        --arg c "$(printf '# Home\n\nwiki page\n' | base64 -w0)" '{title:"Home", content_base64:$c, message:"wiki home"}')" >/dev/null
    curl_api -X POST "$api/repos/$USER_NAME/$R/wiki/new" -d "$(jq -n \
        --arg c "$(printf 'second\n' | base64 -w0)" '{title:"Second Page", content_base64:$c}')" >/dev/null
    curl_api "$api/repos/$USER_NAME/$R/wiki/page/Home" | jq -e '.title == "Home"' >/dev/null
    curl_api "$api/repos/$USER_NAME/$R/wiki/pages" | jq -e 'length == 2' >/dev/null
    [ "$(http_get "/$USER_NAME/$R/wiki/Second-Page" "$work/page.html")" = 200 ]
    cg "$C" clone -q "$(https_url "$R.wiki")" "$W/wiki"
    [ -f "$W/wiki/Home.md" ]
}

s_fork() {
    curl_api -X POST "$api/repos/$USER_NAME/$R/forks" -d "$(jq -n --arg n "$R-fork" '{name:$n}')" >/dev/null
    wait_for 30 sh -c "curl -fsS --cacert '$work/certs/ca.pem' -H 'Authorization: token $token' \
        '$api/repos/$USER_NAME/$R-fork/branches/main' >/dev/null"
    cg "$C" ls-remote "$(ssh_url "$R-fork")" refs/heads/main | grep -q "$(cg "$C" ls-remote "$(https_url "$R")" refs/heads/main | cut -f1)"
}

mirror_has() {
    curl_api "$api/repos/$USER_NAME/$R-mirror/branches/main" | jq -e --arg id "$1" '.commit.id == $id' >/dev/null
}

s_mirror() {
    local head
    curl_api -X POST "$api/repos/migrate" -d "$(jq -n --arg n "$R-mirror" \
        --arg u "https://127.0.0.1:3000/$USER_NAME/$R.git" --arg o "$USER_NAME" --arg t "$token" \
        '{clone_addr:$u, repo_name:$n, repo_owner:$o, mirror:true, auth_token:$t, service:"git", wiki:true}')" >/dev/null
    head=$(cg "$C" ls-remote "$(https_url "$R")" refs/heads/main | cut -f1)
    wait_for 30 mirror_has "$head" || fail "mirror lacks main $head"
    # A new commit on the source, then a mirror sync.
    cd "$W/https"
    cg "$C" fetch -q origin
    cg "$C" checkout -q -B main origin/main
    printf 'for the mirror\n' >mirror.txt
    cg "$C" add mirror.txt
    cg "$C" commit -q -m "for the mirror"
    cg "$C" push -q origin main
    head=$(cg "$C" rev-parse HEAD)
    curl_api -X POST "$api/repos/$USER_NAME/$R-mirror/mirror-sync" >/dev/null
    wait_for 30 mirror_has "$head" || fail "mirror-sync did not bring $head"
}

s_push_create() {
    local d=$W/pushcreate
    cg "$C" init -q "$d"
    printf 'push to create\n' >"$d/file.txt"
    cg "$C" -C "$d" add file.txt
    cg "$C" -C "$d" commit -q -m "push to create"
    cg "$C" -C "$d" push "$(https_url "$R-new")" main
    wait_for 20 sh -c "curl -fsS --cacert '$work/certs/ca.pem' -H 'Authorization: token $token' '$api/repos/$USER_NAME/$R-new' | jq -e '.empty == false'"
}

# --- maintenance and verification (once) ------------------------------------
s_cron() {
    local task
    curl_api "$api/admin/cron" | jq -r '.[].name' | tr '\n' ' '
    echo
    for task in repo_health_check git_gc_repos resync_all_hooks reinit_missing_repos; do
        curl_api -X POST "$api/admin/cron/$task" >/dev/null || fail "cron $task"
    done
    # The tasks run in the background; wait until they ran.
    sleep 10
    curl_api "$api/admin/cron" | jq -e 'map(select(.name == "git_gc_repos" or .name == "repo_health_check")) | all(.exec_times >= 1)' >/dev/null
}

s_server_fsck() {
    local d bad=0
    for d in $(docker exec "$container" sh -c 'ls -d /data/git/repositories/*/*.git'); do
        if docker exec -u git "$container" refgit --git-dir="$d" fsck --strict --no-dangling >"$work/fsck.out" 2>&1; then
            echo "ok   $d"
        else
            echo "FAIL $d"
            sed 's/^/     /' "$work/fsck.out"
            bad=1
        fi
    done
    return $bad
}

# --- run ------------------------------------------------------------------------
for C in $clients; do
    R=repo-$C
    W=$work/w-$C
    mkdir -p "$W"
    log "client $C (repository $R)"
    step "$C:create-repo-auto-init" s_create_repo
    step "$C:clone-https" s_clone_https
    step "$C:push-https" s_push_https
    step "$C:push-ssh-branch-and-tag" s_push_ssh
    step "$C:clone-ssh" s_clone_ssh
    step "$C:builtin-ssh-server" s_builtin_ssh
    step "$C:protocol-v0-https" s_protocol_v0_https
    step "$C:protocol-v0-ssh" s_protocol_v0_ssh
    step "$C:api-file-edits" s_api_files
    step "$C:fetch-https-ssh" s_fetch
    step "$C:api-views" s_api_views
    step "$C:web-views" s_web_views
    step "$C:archives" s_archives
    step "$C:tags-and-releases" s_tags_releases
    step "$C:branches-api" s_branches_api
    for style in merge rebase rebase-merge squash fast-forward-only; do
        step "$C:pr-$style" s_pr "$style"
    done
    step "$C:pr-conflict" s_pr_conflict
    step "$C:agit" s_agit
    step "$C:protected-branch" s_protected_branch
    step "$C:git-hook-pre-receive" s_git_hook
    step "$C:wiki" s_wiki
    step "$C:fork" s_fork
    step "$C:mirror" s_mirror
    step "$C:push-to-create" s_push_create
done
log "maintenance and verification"
step "cron-maintenance" s_cron
step "server-fsck-reference-git" s_server_fsck

# --- report ---------------------------------------------------------------------
docker logs "$container" >"$work/gitea.log" 2>&1 || true
docker cp "$container:/data/git-calls.log" "$work/git-calls.log" >/dev/null 2>&1 || true
log "git calls made by Gitea ($server)"
if [ -f "$work/git-calls.log" ]; then
    echo "total: $(grep -c '^start ' "$work/git-calls.log")"
    echo "by command (calls, failed):"
    # The command is the program name of a dashed call, else the first
    # argument that is not a global option (-c and -C take a value).
    grep -E '^(start |rc=)' "$work/git-calls.log" | awk '{
        rc = substr($1, 4); line = $0; sub(/^[^[]*\[/, "", line)
        k = split(line, t, /\] \[/); sub(/\]$/, "", t[k]); cmd = t[1]
        if (cmd == "git") { cmd = "-"; for (i = 2; i <= k; i++) {
            if (t[i] == "-c" || t[i] == "-C") { i++; continue }
            if (t[i] ~ /^-/) continue
            cmd = t[i]; break } }
        if ($1 == "start") n[cmd]++; else if (rc != 0) f[cmd]++
    } END { for (c in n) printf "  %-22s %5d %5d\n", c, n[c], f[c] }' | sort
    echo "failed calls:"
    awk '/^### step / {step = $3; next}
        /^rc=[1-9]/ {print "(" step ") " $0; show = 1; next}
        /^(rc=0|start )/ {show = 0} show' "$work/git-calls.log" |
        sed -E 's/pwd=[^ ]* //' | sort | uniq -c | sort -rn | head -n 150
fi
if [ -n "$inventory" ] && [ -f "$work/git-calls.log" ]; then
    # One line per distinct form: count, command, options. Gitea's own
    # global options (-c protocol.version=2 -c credential.helper=) are
    # dropped; ids, paths, refs, URLs and free text are masked.
    grep '^start ' "$work/git-calls.log" | awk '
    function mask(v) {
        if (v ~ /^[0-9a-f]{7,64}$/) return "<id>"
        if (v ~ /:\/\//) return "<url>"
        if (v ~ /^refs\// || v ~ /^\+?[^ ]*:refs\//) return "<ref>"
        if (v ~ /^\// || v ~ /^\.merge_file_/) return "<path>"
        return "<arg>"
    }
    {
        line = $0; sub(/^[^[]*\[/, "", line)
        k = split(line, t, /\] \[/); sub(/\]$/, "", t[k])
        i = 2; cmd = t[1]; shape = ""
        if (cmd == "git") {
            for (; i <= k; i++) {
                if (t[i] == "-c" && (t[i+1] == "protocol.version=2" || t[i+1] == "credential.helper=")) { i++; continue }
                if (t[i] == "-c" || t[i] == "-C") { shape = shape " " t[i] " " (t[i] == "-C" ? "<path>" : t[i+1]); i++; continue }
                if (t[i] ~ /^-/) { v = t[i]; sub(/=\/.*/, "=<path>", v); shape = shape " " v; continue }
                break
            }
            cmd = t[i]; i++
        }
        for (; i <= k; i++) {
            v = t[i]
            if (v ~ /^-/) {
                sub(/=\/.*/, "=<path>", v)
                if (v ~ /^--(message|author|date|committer|since|until)=/) sub(/=.*/, "=<arg>", v)
                else if (v ~ /=/) { n = v; sub(/=.*/, "", n); val = v; sub(/^[^=]*=/, "", val)
                    if (val ~ /^[0-9a-f]{7,64}$/ || val ~ /^\// || val ~ /:\/\//) v = n "=" mask(val) }
                shape = shape " " v
            } else if (cmd == "config" && shape ~ /--(get|add|unset-all|replace-all)$/ || cmd == "config" && shape == " --global") {
                shape = shape " " v
            } else {
                shape = shape " " mask(v)
            }
        }
        gsub(/( <arg>)+/, " <arg>...", shape)
        print cmd "\t" substr(shape, 2)
    }' | sort | uniq -c | sort -k2,2 -k1,1rn >"$inventory"
    echo "inventory of call forms: $inventory ($(wc -l <"$inventory") forms)"
fi
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    {
        echo "### Gitea with $server as server git"
        echo
        echo "passed: ${#passed[@]}, failed: ${#failed[@]}, known failures: ${#expected[@]}, newly passing: ${#fixed[@]}"
        for s in "${failed[@]}"; do echo "- FAIL $s"; done
        for s in "${expected[@]}"; do echo "- XFAIL $s"; done
        for s in "${fixed[@]}"; do echo "- XPASS $s"; done
    } >>"$GITHUB_STEP_SUMMARY"
fi

log "summary (server git: $server)"
echo "passed:        ${#passed[@]}"
echo "known failure: ${#expected[@]} ${expected[*]}"
echo "newly passing: ${#fixed[@]} ${fixed[*]}"
echo "failed:        ${#failed[@]} ${failed[*]}"
[ "${#failed[@]}" -eq 0 ] || die "unexpected failures: ${failed[*]}"
