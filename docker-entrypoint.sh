#!/bin/sh
set -e

# Default UID/GID if not provided
USER_ID=${PUID:-99}
GROUP_ID=${PGID:-100}

# Update user/group to match runtime IDs
echo "Setting up user with UID:${USER_ID} GID:${GROUP_ID}" >&2

# Remove existing smugmug user
deluser smugmug 2>/dev/null || true

# Check if requested GID already exists, use it or create new group
if getent group ${GROUP_ID} > /dev/null 2>&1; then
    GROUP_NAME=$(getent group ${GROUP_ID} | cut -d: -f1)
    echo "Using existing group '${GROUP_NAME}' with GID ${GROUP_ID}" >&2
else
    addgroup -g ${GROUP_ID} smugmug >&2
    GROUP_NAME="smugmug"
fi

# Create user with specified UID and existing/new group
adduser -D -u ${USER_ID} -G ${GROUP_NAME} smugmug >/dev/null 2>&1

# Ensure directories exist and have correct ownership
mkdir -p /home/smugmug/.config/smugmug-cli /home/smugmug/.cache/smugmug-cli >/dev/null 2>&1
chown -R smugmug:${GROUP_NAME} /home/smugmug >/dev/null 2>&1

# Run at low CPU priority (NICE, default 10) and, where the kernel allows,
# idle I/O priority (IONICE_CLASS, default 3 = idle; set empty to skip), so
# a backup yields to everything else on the host.
PRIORITY=""
if [ -n "${NICE-10}" ]; then
    PRIORITY="nice -n ${NICE-10}"
fi
if [ -n "${IONICE_CLASS-3}" ] && command -v ionice >/dev/null 2>&1 \
    && ionice -c "${IONICE_CLASS-3}" true 2>/dev/null; then
    PRIORITY="$PRIORITY ionice -c ${IONICE_CLASS-3}"
fi

# Drop privileges and execute the command (exec, so it gets docker stop's
# SIGTERM and finishes its uploads in progress)
exec su-exec smugmug $PRIORITY "$@"
