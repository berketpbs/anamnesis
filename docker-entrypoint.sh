#!/bin/bash
set -e

# Default to serving if no command given
if [ $# -eq 0 ]; then
    set -- anamnesis serve --bind 0.0.0.0 --port 8080
    # A container has to bind 0.0.0.0 to be reachable through a published port.
    # Whether that port is published, and to whom, is decided on the host. Say
    # what is unguarded and start anyway, rather than refusing a `docker run`
    # nobody could fix from in here.
    if [ -z "${ANAMNESIS_TOKEN}" ] && [ -z "${ANAMNESIS_TOKENS}" ]; then
        echo "anamnesis: no ANAMNESIS_TOKEN set - every caller reaching this port is accepted" >&2
        set -- "$@" --allow-anonymous
    fi
fi

# Create .anamnesis directory if it doesn't exist
mkdir -p "${ANAMNESIS_DATA_DIR:=/root/.anamnesis}"

# Run the command
exec "$@"
