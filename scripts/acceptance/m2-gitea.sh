#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
#
# M2 acceptance test (issue #163): clone -> edit -> commit -> push against a
# Gitea server, over HTTPS (token through a credential helper and through
# askpass) and over SSH (OpenSSH, key authentication).
#
# The server runs in a throwaway Gitea container (pinned image, real git
# inside). The client commands in scenario.sh run under `env -i` with a PATH
# that holds only rgit (as `git`), a few shell utilities and ssh: there is no
# other git to fall back to. The server side is checked with the reference git
# inside the container: `git fsck --strict`, the expected refs and the expected
# commit ids (the scenario commits with fixed identities and dates, so the ids
# are the same for every transport and for every conforming client).
#
# Usage: scripts/acceptance/m2-gitea.sh [options]
#   --rgit <path>              client under test (default: target/release/rgit)
#   --reference-client <path>  run the scenario with this git instead of rgit,
#                              to check the scenario itself against git
#   --modes "<list>"           subset of: https-helper https-askpass ssh
#   --keep                     keep the container and the work directory
#
# Needs on the host: docker, curl, openssl, ssh, ssh-keygen. The container
# gets a unique name and random free host ports (bound to 127.0.0.1) and is
# removed with its volumes on exit.
set -euo pipefail

GITEA_IMAGE=${GITEA_IMAGE:-gitea/gitea:1.24.6}
USER_NAME=rgit-ci
USER_PASS="accept-$RANDOM-$RANDOM-pw"

# Expected commit ids. They depend only on the scenario's content, identities
# and dates; `--reference-client git` must produce the same ids.
EXPECT_SEED=0b930a508e0f85fbd41667afa3c8465d663e59ad
EXPECT_C1=fcf7c8831f3224d88b6b3cc906f501ae4d7c383d
EXPECT_C2=786b4f18ef8c9ade5929c30afda4005a55737c4c
EXPECT_SERVER1=f7c6366a32753d74050859d44819f0b0205105ab
EXPECT_C3=5a11f8cfbfcd1b4c232fd4814502be3d8d957cb4
EXPECT_C4=851da3792073eed13580b91ffaf31f37e6a073ba

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(cd "$here/../.." && pwd)
client=$root/target/release/rgit
reference=0
modes="https-helper https-askpass ssh"
keep=0
while [ $# -gt 0 ]; do
    case "$1" in
    --rgit) client=$2; shift 2 ;;
    --reference-client) client=$2; reference=1; shift 2 ;;
    --modes) modes=$2; shift 2 ;;
    --keep) keep=1; shift ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done
client=$(cd "$(dirname "$client")" && pwd)/$(basename "$client")
[ -x "$client" ] || { echo "client not found: $client (cargo build --release -p rgit-cli)" >&2; exit 2; }

work=$(mktemp -d "${TMPDIR:-/tmp}/rgit-accept.XXXXXX")
container=rgit-accept-$(date +%s)-$$-$RANDOM
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
log "certificates"
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

# --- Gitea -------------------------------------------------------------------
log "gitea ($GITEA_IMAGE) as $container"
docker create --name "$container" \
    -p 127.0.0.1::3000 -p 127.0.0.1::22 \
    -e GITEA__security__INSTALL_LOCK=true \
    -e GITEA__database__DB_TYPE=sqlite3 \
    -e GITEA__server__PROTOCOL=https \
    -e GITEA__server__CERT_FILE=/certs/cert.pem \
    -e GITEA__server__KEY_FILE=/certs/key.pem \
    -e GITEA__server__ROOT_URL=https://127.0.0.1:3000/ \
    -e GITEA__server__LFS_START_SERVER=false \
    -e GITEA__service__DISABLE_REGISTRATION=true \
    -e GITEA__log__LEVEL=Warn \
    "$GITEA_IMAGE" >/dev/null
docker cp "$work/certs/." "$container:/certs"
docker start "$container" >/dev/null
http_port=$(docker port "$container" 3000/tcp | head -n 1 | sed 's/.*://')
ssh_port=$(docker port "$container" 22/tcp | head -n 1 | sed 's/.*://')
base=https://127.0.0.1:$http_port
api=$base/api/v1
echo "https port $http_port, ssh port $ssh_port"

for _ in $(seq 120); do
    curl -fsS --cacert "$work/certs/ca.pem" "$base/api/healthz" >/dev/null 2>&1 && break
    sleep 1
done
curl -fsS --cacert "$work/certs/ca.pem" "$base/api/healthz" >/dev/null ||
    { docker logs "$container" | tail -n 50; die "gitea did not come up"; }

gitea_exec() { docker exec -u git "$container" "$@"; }
curl_api() { curl -fsS --cacert "$work/certs/ca.pem" -H 'Content-Type: application/json' "$@"; }
json_field() { sed -n "s/.*\"$1\":\"\\([^\"]*\\)\".*/\\1/p"; }

log "user, token, ssh key"
gitea_exec gitea admin user create --username "$USER_NAME" --password "$USER_PASS" \
    --email rgit-ci@example.com --admin --must-change-password=false >/dev/null
token=$(curl_api -u "$USER_NAME:$USER_PASS" -X POST "$api/users/$USER_NAME/tokens" \
    -d '{"name":"acceptance","scopes":["write:repository","write:user"]}' | json_field sha1)
[ -n "$token" ] || die "no access token"
printf '%s\n' "$token" >"$work/token"
ssh-keygen -q -t ed25519 -N '' -C rgit-acceptance -f "$work/id_ed25519"
curl_api -H "Authorization: token $token" -X POST "$api/user/keys" \
    -d "{\"title\":\"acceptance\",\"key\":\"$(cat "$work/id_ed25519.pub")\"}" >/dev/null
host_key=$(docker exec "$container" cat /data/ssh/ssh_host_ed25519_key.pub | cut -d' ' -f1,2)
printf '[127.0.0.1]:%s %s\n' "$ssh_port" "$host_key" >"$work/known_hosts"

# --- the client sandbox: only the client under test is `git` ----------------
sandbox=$work/bin
mkdir -p "$sandbox"
ln -s "$client" "$sandbox/git"
for tool in sh bash cat cut sed grep mv rm mkdir chmod ssh; do
    path=$(command -v "$tool") || die "$tool not found on the host"
    ln -s "$path" "$sandbox/$tool"
done

# Credential helper and askpass program for the HTTPS modes. Both hand out the
# token; neither is a git program. Each call is logged, so that the test can
# tell that the credentials really came from them.
cat >"$work/credential-helper" <<EOF
#!/bin/sh
echo "\$1" >>'$work/credential-helper.calls'
test "\$1" = get || exit 0
cat >/dev/null
printf 'username=%s\n' '$USER_NAME'
printf 'password=%s\n' "\$(cat '$work/token')"
EOF
cat >"$work/askpass" <<EOF
#!/bin/sh
echo "\$1" >>'$work/askpass.calls'
case "\$1" in
Username*) printf '%s\n' '$USER_NAME' ;;
Password*) cat '$work/token' ;;
*) exit 1 ;;
esac
EOF
chmod +x "$work/credential-helper" "$work/askpass"

# --- server-side helpers (reference git inside the container) ----------------
repo_dir() { echo "/data/git/repositories/$USER_NAME/$1.git"; }

server_git_env=(-e GIT_AUTHOR_NAME="Server Side" -e GIT_AUTHOR_EMAIL=server@example.com
    -e GIT_COMMITTER_NAME="Server Side" -e GIT_COMMITTER_EMAIL=server@example.com
    -e GIT_CONFIG_NOSYSTEM=1 -e HOME=/tmp)

# The server-side commits are made with the reference git inside the
# container and pushed to Gitea over its own HTTPS port, so that Gitea's hooks
# and database see them like any other push.
server_url() { echo "https://$USER_NAME:$token@127.0.0.1:3000/$USER_NAME/$1.git"; }

# seed <repo>: the first commit of the repository.
seed() {
    docker exec -u git "${server_git_env[@]}" -e REPO_URL="$(server_url "$1")" \
        -e GIT_SSL_NO_VERIFY=1 \
        -e GIT_AUTHOR_DATE="1700000000 +0000" -e GIT_COMMITTER_DATE="1700000000 +0000" \
        "$container" sh -ec '
        d=$(mktemp -d); git init -q -b main "$d/w"; cd "$d/w"
        printf "# accept\n\nfirst line\nsecond line\nthird line\n" >README.md
        printf "obsolete\n" >obsolete.txt
        mkdir src; printf "alpha\nbeta\ngamma\ndelta\nepsilon\n" >src/lib.txt
        printf "#!/bin/sh\necho tool\n" >tool.sh; chmod +x tool.sh
        git add . && git commit -q -m seed
        git push -q "$REPO_URL" main 2>&1
        rm -rf "$d"'
}

# server_change <repo>: a commit on main made on the server.
server_change() {
    docker exec -u git "${server_git_env[@]}" -e REPO_URL="$(server_url "$1")" \
        -e GIT_SSL_NO_VERIFY=1 \
        -e GIT_AUTHOR_DATE="1700004000 +0000" -e GIT_COMMITTER_DATE="1700004000 +0000" \
        "$container" sh -ec '
        d=$(mktemp -d); git clone -q "$REPO_URL" "$d/w"; cd "$d/w"
        printf "server line\n" >>README.md
        git commit -q -a -m "server change"
        git push -q origin main 2>&1
        rm -rf "$d"'
}

# expect_id <name> <expected> <actual>
expect_id() {
    if [ "$EXPECT_C1" = 0000000000000000000000000000000000000000 ]; then
        echo "  $1 = $3 (no expectation recorded)"
    elif [ "$2" != "$3" ]; then
        die "commit id $1: expected $2, got $3"
    fi
}

verify_server() {
    local repo=$1 state=$2 dir
    dir=$(repo_dir "$repo")
    log "$repo: server-side verification with the reference git"
    gitea_exec git --git-dir="$dir" fsck --strict --no-dangling ||
        die "$repo: git fsck --strict failed"
    local refs expected
    refs=$(gitea_exec git --git-dir="$dir" for-each-ref --format='%(refname) %(objectname)')
    expected="refs/heads/main $(cat "$state/c3")"
    [ "$refs" = "$expected" ] || die "$repo: refs: expected '$expected', got '$refs'"
    [ "$(gitea_exec git --git-dir="$dir" symbolic-ref HEAD)" = refs/heads/main ] ||
        die "$repo: HEAD is not refs/heads/main"
    # History of main on the server: c3 <- server change <- c1 <- seed.
    local history
    history=$(gitea_exec git --git-dir="$dir" log --format='%H %s' refs/heads/main)
    [ "$history" = "$(cat "$state/c3") after fetch
$(cat "$state/server1") server change
$(cat "$state/c1") edit files
$(cat "$state/seed") seed" ] || die "$repo: unexpected history on main:
$history"
    # The forced-pushed feature commit reached the server before the deletion.
    gitea_exec git --git-dir="$dir" cat-file -e "$(cat "$state/c4")^{commit}" ||
        die "$repo: forced-pushed commit missing on the server"
    expect_id seed "$EXPECT_SEED" "$(cat "$state/seed")"
    expect_id c1 "$EXPECT_C1" "$(cat "$state/c1")"
    expect_id c2 "$EXPECT_C2" "$(cat "$state/c2")"
    expect_id server1 "$EXPECT_SERVER1" "$(cat "$state/server1")"
    expect_id c3 "$EXPECT_C3" "$(cat "$state/c3")"
    expect_id c4 "$EXPECT_C4" "$(cat "$state/c4")"
}

# run_phase <mode> <state> <url> <phase>
run_phase() {
    local mode=$1 state=$2 url=$3 phase=$4
    local -a env=(
        PATH="$sandbox" HOME="$state/home" XDG_CONFIG_HOME="$state/home/.config"
        GIT_CONFIG_NOSYSTEM=1 LC_ALL=C TZ=UTC GIT_TERMINAL_PROMPT=0
        GIT_AUTHOR_NAME="Acceptance Tester" GIT_AUTHOR_EMAIL=tester@example.com
        GIT_COMMITTER_NAME="Acceptance Tester" GIT_COMMITTER_EMAIL=tester@example.com
        GIT_SSL_CAINFO="$work/certs/ca.pem"
        ACCEPT_URL="$url" ACCEPT_STATE="$state" ACCEPT_REFERENCE="$reference"
    )
    case "$mode" in
    https-askpass) env+=(GIT_ASKPASS="$work/askpass") ;;
    ssh)
        env+=(GIT_SSH_COMMAND="ssh -F /dev/null -i $work/id_ed25519 -o IdentitiesOnly=yes -o BatchMode=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile=$work/known_hosts")
        ;;
    esac
    env -i "${env[@]}" "$sandbox/bash" "$here/scenario.sh" "$phase"
}

# check_credential_source <mode>: the HTTPS credentials came from the helper
# or from askpass, and the other source was never asked.
check_credential_source() {
    local used=$work/credential-helper.calls unused=$work/askpass.calls
    case "$1" in
    https-askpass) used=$work/askpass.calls unused=$work/credential-helper.calls ;;
    ssh) used="" ;;
    esac
    if [ -n "$used" ]; then
        [ -s "$used" ] || die "$1: $(basename "$used" .calls) was never called"
        grep -q . "$unused" 2>/dev/null && die "$1: $(basename "$unused" .calls) was called"
    elif [ -e "$work/credential-helper.calls" ] || [ -e "$work/askpass.calls" ]; then
        die "ssh: HTTPS credentials were asked for"
    fi
    return 0
}

failed=""
for mode in $modes; do
    repo=accept-$mode
    state=$work/$mode
    mkdir -p "$state/home/.config"
    case "$mode" in
    https-helper)
        url=$base/$USER_NAME/$repo.git
        printf '[credential]\n\thelper = %s\n' "$work/credential-helper" >"$state/home/.gitconfig"
        ;;
    https-askpass) url=$base/$USER_NAME/$repo.git ;;
    ssh) url=ssh://git@127.0.0.1:$ssh_port/$USER_NAME/$repo.git ;;
    *) die "unknown mode $mode" ;;
    esac
    log "$mode: repository $repo"
    curl_api -H "Authorization: token $token" -X POST "$api/user/repos" \
        -d "{\"name\":\"$repo\",\"private\":true,\"default_branch\":\"main\"}" >/dev/null
    seed "$repo"
    # The repository is private: without credentials the server refuses.
    anon=$(curl -s -o /dev/null -w '%{http_code}' --cacert "$work/certs/ca.pem" \
        "$base/$USER_NAME/$repo.git/info/refs?service=git-upload-pack")
    [ "$anon" = 401 ] || die "$repo: anonymous access answered $anon, expected 401"
    rm -f "$work/credential-helper.calls" "$work/askpass.calls"
    if run_phase "$mode" "$state" "$url" clone &&
        { log "$mode: server-side change"; server_change "$repo"; } &&
        run_phase "$mode" "$state" "$url" fetch &&
        (verify_server "$repo" "$state") &&
        (check_credential_source "$mode"); then
        log "$mode: PASS"
    else
        log "$mode: FAIL"
        failed="$failed $mode"
    fi
done

[ -z "$failed" ] || die "failed modes:$failed"
log "all modes passed ($modes)"
