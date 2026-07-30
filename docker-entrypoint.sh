#!/usr/bin/env bash
# AndrewOS container entrypoint.
# Runs once as root to fix volume ownership and validate config, then drops to
# the unprivileged `tiger` user before launching the server.
set -euo pipefail

if [ "$(id -u)" = "0" ]; then
    # Headless mode requires a token; without stdin it would hang on the prompt.
    if [ -z "${ACCESS_TOKEN:-}" ]; then
        echo "ERROR: ACCESS_TOKEN is not set." >&2
        echo "       Copy .env.example to .env and set a strong token, e.g.:" >&2
        echo "         ACCESS_TOKEN=\$(openssl rand -hex 32)" >&2
        exit 1
    fi

    # Make bind-mounted volumes writable by the tiger user.
    mkdir -p /app/data /app/sandbox
    chown tiger:tiger /app/data /app/sandbox 2>/dev/null || true

    exec gosu tiger "$@"
fi

# Already non-root (e.g. `user:` overridden in compose) — just run.
exec "$@"
