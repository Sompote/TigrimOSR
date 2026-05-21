---
name: env-check
description: Validate environment variables, dependencies, and system requirements. Diagnose missing configs, version mismatches, and setup issues.
---

# Environment Check

## Overview

Validate that a project's environment is correctly configured — check dependencies, environment variables, system tools, ports, and configuration files.

## When to Use

- User asks "why won't this run?" or "check my setup"
- Project fails to start or build
- Verifying deployment readiness
- Onboarding a new developer
- Debugging "works on my machine" issues

## Workflow

1. **Identify project type** — read package.json, Cargo.toml, requirements.txt, etc.
2. **Check dependencies** — are they installed? correct versions?
3. **Check environment variables** — are required vars set?
4. **Check system tools** — right versions of runtime/compiler?
5. **Check ports/services** — are required services running?
6. **Report findings** with fix instructions

## Check Commands

### Dependencies

```bash
# Node.js
node --version
npm --version
npm ls --depth=0 2>&1 | head -30

# Python
python3 --version
pip3 list 2>/dev/null | head -30

# Rust
rustc --version
cargo --version

# System tools
git --version
docker --version 2>/dev/null
```

### Environment Variables

```bash
# Check if specific vars are set (without leaking values)
env | grep -E "^(DATABASE_URL|API_KEY|SECRET|TOKEN|PORT|HOST|NODE_ENV)" | sed 's/=.*/=<set>/'

# Check .env file exists
ls -la .env .env.local .env.production 2>/dev/null
```

### Ports & Services

```bash
# Check if a port is in use
lsof -i :3000 2>/dev/null
lsof -i :5432 2>/dev/null
lsof -i :6379 2>/dev/null

# Check common services
pg_isready 2>/dev/null && echo "PostgreSQL: OK" || echo "PostgreSQL: not running"
redis-cli ping 2>/dev/null && echo "Redis: OK" || echo "Redis: not running"
```

### File System

```bash
# Check required directories exist
ls -d data/ config/ logs/ tmp/ 2>/dev/null

# Check file permissions
ls -la .env config/*.json 2>/dev/null

# Check disk space
df -h . | tail -1
```

## Project-Specific Checks

### Node.js Project
1. `node --version` matches engines in package.json
2. `node_modules/` exists (run `npm install` if not)
3. Required env vars from `.env.example` are set
4. Build artifacts exist if needed (`dist/`, `build/`)

### Rust Project
1. `rustc --version` meets edition requirement
2. `cargo build` succeeds
3. Required system libraries installed (openssl, etc.)
4. Feature flags correct in Cargo.toml

### Python Project
1. Correct Python version
2. Virtual environment activated
3. `pip install -r requirements.txt` complete
4. Required system packages (libpq-dev, etc.)

## Output Format

```
## Environment Check Results

[OK]   Node.js v22.16.0
[OK]   npm 10.8.0
[FAIL] DATABASE_URL not set — required for database connection
       Fix: export DATABASE_URL="postgresql://user:pass@localhost:5432/db"
[WARN] Port 3000 already in use by process 12345
       Fix: kill 12345 or change PORT in .env
[OK]   All npm dependencies installed
[FAIL] Missing Python package: duckduckgo-search
       Fix: pip install duckduckgo-search
```

## Best Practices

- Never print actual secret values — just confirm they're set
- Check `.env.example` or `.env.template` for required variable list
- Suggest exact fix commands, not just "fix this"
- Check both dev and production requirements
- Verify version ranges, not just presence (node 18+ vs node 14)
