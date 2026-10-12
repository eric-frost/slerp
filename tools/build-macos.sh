#!/bin/bash
# Builds Slerp on a Mac over SSH and packs Slerp.app, run from Linux.
#
#   SLERP_MAC=user@host tools/build-macos.sh            # release build, app, zip
#   SLERP_MAC=user@host tools/build-macos.sh install    # ...and install it there
#   SLERP_MAC=user@host tools/build-macos.sh uninstall  # remove it from the Mac
#   SLERP_MAC=user@host tools/build-macos.sh cargo test -p slerp-helper
#   SLERP_MAC=user@host tools/build-macos.sh run qlmanage -m   # any command there
#
# "install" puts Slerp.app in the Mac user's ~/Applications and
# registers its file types and Quick Look thumbnail extension; the steps
# are in tools/macos-app.sh, which also works on a Mac directly.
#
# The Mac must see this folder at the same path (through Syncthing, say), so
# this first waits until the Mac's copy of the source matches this one.
# local/ is not shared: the Mac builds into its own local/target, and the
# bundle and zip are made in its local/dist, then copied to local/dist here.
# Builds run two jobs at a time (SLERP_MAC_JOBS), which suits a Mac with
# 8 GB of memory; never run two builds there at once.
#
# SSH: use keys, or set SSHPASS for a password login (it is passed through
# sshpass and never written anywhere).  SLERP_MAC_CREDENTIALS may instead
# name a file holding the host's line followed by a "password: ..." line;
# the password stays in this script's own environment.  The script never
# lets ssh ask for a password: a missing one fails at once instead of
# opening a password dialog on the desktop.  Settings for one machine can go
# in local/build-macos.env, which is read first.
set -euo pipefail
cd "$(dirname "$0")/.."
if [ -r local/build-macos.env ]; then
  . local/build-macos.env
fi
HOST=${SLERP_MAC:?set SLERP_MAC to user@host of the Mac}
DIR=$PWD
JOBS=${SLERP_MAC_JOBS:-2}
MODE=""
case "${1:-}" in
  install | uninstall) MODE=$1 ;;
esac
SSH_OPTS=(-o ConnectTimeout=5 -o StrictHostKeyChecking=accept-new -o LogLevel=ERROR)

# The password from the credentials file, when none was given.
CREDENTIALS=${SLERP_MAC_CREDENTIALS:-}
if [ -z "${SSHPASS:-}" ] && [ -n "$CREDENTIALS" ] && [ -r "$CREDENTIALS" ]; then
  SSHPASS=$(awk -v host="$HOST" '$0 == host { found = 1; next } found && /^password:/ { sub(/^password: */, ""); print; exit }' "$CREDENTIALS")
  export SSHPASS
fi
# Never let ssh or scp ask: with no password, key login or failure.
export SSH_ASKPASS_REQUIRE=never
if [ -z "${SSHPASS:-}" ]; then
  SSH_OPTS+=(-o BatchMode=yes)
fi

remote() {
  if [ -n "${SSHPASS:-}" ]; then
    SSH_ASKPASS_REQUIRE=never sshpass -e ssh "${SSH_OPTS[@]}" "$HOST" "$@"
  else
    ssh "${SSH_OPTS[@]}" "$HOST" "$@"
  fi
}

copy() {
  if [ -n "${SSHPASS:-}" ]; then
    SSH_ASKPASS_REQUIRE=never sshpass -e scp -q "${SSH_OPTS[@]}" "$@"
  else
    scp -q "${SSH_OPTS[@]}" "$@"
  fi
}

# 1. Wait for Syncthing: the same list of files with the same contents.
#    Mac droppings and Syncthing's temporary files don't count.
MANIFEST='find Cargo.toml Cargo.lock rustfmt.toml README.md ARCHITECTURE.md .cargo src crates data tools -type f \
  ! -name .DS_Store ! -name "._*" ! -name ".syncthing.*" -print0 | LC_ALL=C sort -z | xargs -0 shasum | shasum'
for i in $(seq 1 60); do
  # This side is read again each time, so edits made meanwhile (another
  # agent's, say) are waited for rather than never matched.
  here=$(bash -c "$MANIFEST")
  there=$(remote "cd '$DIR' && $MANIFEST" 2>/dev/null || true)
  [ "$here" = "$there" ] && break
  [ "$i" = 1 ] && echo "waiting for Syncthing to bring the source to the Mac..."
  sleep 5
done
if [ "$here" != "$there" ]; then
  echo "error: the Mac's copy of the source still differs after 5 minutes" >&2
  exit 1
fi

# 2. Any cargo command, as a login shell so the toolchain's PATH is set.
#    --locked keeps the Mac from rewriting Cargo.lock, which Syncthing would
#    carry back here.  The deployment target is tools/macos-app.sh's, so
#    the two share build output.
cargo_remote() {
  local sub=$1
  shift
  remote "zsh -lc 'cd \"$DIR\" && MACOSX_DEPLOYMENT_TARGET=11.0 ~/.cargo/bin/cargo $sub --locked --jobs $JOBS $*'"
}
if [ "${1:-}" = cargo ]; then
  shift
  cargo_remote "$@"
  exit
fi

# 2a. Any other command, in the project folder as a login shell, for tests
#     on the Mac (qlmanage, pluginkit, the installed app's command line).
#     The arguments are joined into one shell command there.
if [ "${1:-}" = run ]; then
  shift
  remote "zsh -lc $(printf %q "cd '$DIR' && $*")"
  exit
fi

# 3. The icon, rendered here (the Mac has no SVG renderer on the command
#    line) and turned into an .icns there.
mkdir -p local/dist
if [ "$MODE" != uninstall ] && command -v inkscape >/dev/null; then
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  if inkscape data/slerp.svg -w 1024 -h 1024 -o "$tmp/icon.png" >/dev/null 2>&1; then
    remote "mkdir -p '$DIR/local/dist'"
    copy "$tmp/icon.png" "$HOST:$DIR/local/dist/icon-1024.png"
  fi
fi

# 4. The app, its Quick Look extension and the zip, made on the Mac by
#    tools/macos-app.sh, which also installs and uninstalls.
remote "zsh -lc 'cd \"$DIR\" && SLERP_MAC_JOBS=$JOBS tools/macos-app.sh $MODE'"
[ "$MODE" = uninstall ] && exit

# 5. Bring the zip back.
mkdir -p local/dist
copy "$HOST:$DIR/local/dist/slerp-macos-arm64.zip" local/dist/
ls -l local/dist/slerp-macos-arm64.zip
