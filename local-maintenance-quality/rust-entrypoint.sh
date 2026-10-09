#!/usr/bin/env bash
# Provision a disposable account; all Cargo/test writes run as the operator uid.
set -euo pipefail
quality_uid=$1
quality_gid=$2
shift 2
if ! getent group "$quality_gid" >/dev/null; then
  groupadd --gid "$quality_gid" quality-build
fi
if ! getent passwd "$quality_uid" >/dev/null; then
  useradd --uid "$quality_uid" --gid "$quality_gid" --create-home quality-build
fi
if [[ ${1:-} == --host-tests ]]; then
  shift
  # Existing native-install permission tests expect the host posture. Remove
  # only the disposable container markers, never files in the mounted worktree.
  rm -f /.dockerenv /run/.containerenv
fi
quality_user=$(getent passwd "$quality_uid" | cut -d: -f1)
# runuser gives libraries the disposable account's real home as well as its uid.
exec runuser --user "$quality_user" -- cargo "$@"
