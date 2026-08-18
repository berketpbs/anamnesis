#!/bin/bash
set -e

# Default to serving if no command given
if [ $# -eq 0 ]; then
    set -- anamnesis serve --bind 0.0.0.0 --port 8080
fi

# Create .anamnesis directory if it doesn't exist
mkdir -p "${ANAMNESIS_DB:=/root/.anamnesis}"

# Run the command
exec "$@"
