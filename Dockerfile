# syntax=docker/dockerfile:1
#
# TigrimOS — headless web server in a container.
# Multi-stage: build the Rust binary, then ship it on a slim runtime that has
# the Python/Node/shell tools the agent shells out to at runtime.

# ---------- Stage 1: build the Rust binary ----------
FROM rust:1-bookworm AS builder
WORKDIR /app

# Pre-build dependencies as a cached layer (rebuilds only when Cargo.* change).
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && echo 'fn main() {}' > src/main.rs \
    && cargo build --release \
    && rm -rf src target/release/deps/tigrimos* target/release/tigrimos

# Real sources. assets/ and static/ are needed at compile time (include_str!/include_bytes!).
COPY src ./src
COPY assets ./assets
COPY static ./static
RUN cargo build --release

# ---------- Stage 2: runtime ----------
FROM debian:bookworm-slim AS runtime
ENV DEBIAN_FRONTEND=noninteractive

# Runtime tools the agent may invoke: python3, node, bash/git/curl.
# tini = clean PID 1 / signal handling; gosu = drop root to the tiger user.
RUN apt-get update && apt-get install -y --no-install-recommends \
        python3 python3-venv python3-pip \
        nodejs npm \
        bash git curl ca-certificates tini gosu \
    && rm -rf /var/lib/apt/lists/*

# Python libs used by the bundled skills (web-search, charts, excel, ...).
COPY requirements.txt /tmp/requirements.txt
RUN python3 -m venv /opt/venv \
    && /opt/venv/bin/pip install --no-cache-dir --upgrade pip \
    && /opt/venv/bin/pip install --no-cache-dir -r /tmp/requirements.txt
# Put the venv first on PATH so python_command() (resolves via PATH) finds it.
ENV PATH="/opt/venv/bin:${PATH}"
# The app prepends /usr/local/bin:/usr/bin:… to PATH at startup, which would
# otherwise shadow the venv with the system python3 (missing our deps). Symlink
# the venv interpreters into /usr/local/bin (first in that list) so run_python /
# run_shell resolve the venv python that actually has duckduckgo-search etc.
RUN ln -sf /opt/venv/bin/python3 /usr/local/bin/python3 \
    && ln -sf /opt/venv/bin/python  /usr/local/bin/python \
    && ln -sf /opt/venv/bin/pip     /usr/local/bin/pip \
    && ln -sf /opt/venv/bin/pip3    /usr/local/bin/pip3

# ClawHub skill marketplace CLI (Node). Optional — don't fail the build if unavailable.
RUN npm install -g clawhub && npm cache clean --force \
    || echo "clawhub install skipped (marketplace will be unavailable)"

# Optional: browser control (Playwright). OFF by default to keep the image slim
# (~400 MB of browser + system libs). Enable with:
#   docker compose build --build-arg INSTALL_BROWSER=true   (or set it in .env)
# Browsers install to a shared, world-readable path so the non-root runtime user
# finds them. The container always runs --headless, so auto-headless applies.
ARG INSTALL_BROWSER=false
ENV PLAYWRIGHT_BROWSERS_PATH=/opt/ms-playwright
RUN if [ "$INSTALL_BROWSER" = "true" ]; then \
        echo "Installing browser for browser control…" \
        && npx --yes playwright install-deps chromium \
        && npx --yes @playwright/mcp@latest install-browser chrome-for-testing \
        && chmod -R a+rX /opt/ms-playwright ; \
    else \
        echo "Browser control browser NOT installed (build with --build-arg INSTALL_BROWSER=true)" ; \
    fi

# Non-root user for defence-in-depth (the container itself is the sandbox boundary).
RUN useradd -m -u 1000 -s /bin/bash tiger

WORKDIR /app
COPY --from=builder /app/target/release/tigrimos /usr/local/bin/tigrimos
COPY docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN chmod +x /usr/local/bin/docker-entrypoint.sh \
    && mkdir -p /app/data /app/sandbox \
    && chown -R tiger:tiger /app

# data_dir() prefers ./data when present (here /app/data); sandbox via SANDBOX_DIR.
# MPLBACKEND=Agg lets matplotlib render without a display.
ENV SANDBOX_DIR=/app/sandbox \
    PORT=3001 \
    MPLBACKEND=Agg

EXPOSE 3001

HEALTHCHECK --interval=30s --timeout=5s --start-period=25s --retries=3 \
    CMD curl -fsS http://127.0.0.1:3001/web > /dev/null || exit 1

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/docker-entrypoint.sh"]
CMD ["tigrimos", "--headless"]
