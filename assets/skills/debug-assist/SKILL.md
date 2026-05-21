---
name: debug-assist
description: Intelligent debugging assistant — analyzes error messages, stack traces, logs, and runtime behavior to identify root causes and suggest fixes.
---

# Debug Assistant

## Overview

Analyze errors, stack traces, crash logs, and unexpected behavior to identify root causes and provide actionable fixes.

## When to Use

- User shares an error message or stack trace
- Application crashes or produces wrong output
- "Why is this not working?" type questions
- Performance debugging (slow queries, memory leaks)
- Build or compilation errors

## Workflow

1. **Collect context** — read error messages, logs, relevant source files
2. **Trace the error** — follow the stack trace to the source location
3. **Read surrounding code** using `read_file` at the error location
4. **Identify root cause** — match error patterns to known causes
5. **Suggest fix** with concrete code changes
6. **Verify** — check if the fix could introduce new issues

## Error Analysis Patterns

### Stack Trace Analysis
- Read from bottom to top (most recent call last in Python/JS)
- Read from top to bottom (Rust, Java, Go)
- Focus on frames in USER code, not library internals
- Check the exact line and column referenced

### Common Error Categories

#### Null/Undefined Reference
- **Symptoms:** NullPointerException, "cannot read property of undefined", unwrap() on None
- **Check:** Is the variable initialized? Is the API returning null? Is there a race condition?

#### Type Errors
- **Symptoms:** TypeError, mismatched types, failed casts
- **Check:** Function signatures, API response shapes, serialization/deserialization

#### Import/Module Errors
- **Symptoms:** ModuleNotFoundError, "cannot find module", unresolved import
- **Check:** Package installed? Correct path? Circular dependency?

#### Permission/Access Errors
- **Symptoms:** PermissionDenied, EACCES, 403 Forbidden
- **Check:** File permissions, API keys, authentication tokens, CORS

#### Connection Errors
- **Symptoms:** ConnectionRefused, timeout, ECONNRESET
- **Check:** Service running? Correct host/port? Firewall? SSL cert?

#### Build/Compile Errors
- **Symptoms:** Syntax errors, missing dependencies, version conflicts
- **Check:** Lock files, dependency versions, build tool config

## Debugging Commands

Use `run_shell` to gather diagnostic info:

```bash
# Check if a service is running
lsof -i :PORT

# Check file permissions
ls -la /path/to/file

# Check environment variables
env | grep RELEVANT_VAR

# Check disk space
df -h

# Check memory usage
top -l 1 | head -20

# Check recent logs
tail -100 /path/to/logfile

# Test network connectivity
curl -v http://host:port/health
```

## Output Format

```
## Root Cause
Clear one-sentence explanation of why the error occurs.

## Evidence
- Line X in file.rs: `code that causes the issue`
- Error message indicates: ...
- The variable `foo` is None because ...

## Fix
Concrete code change:
[code snippet]

## Prevention
How to prevent this class of error in the future.
```

## Best Practices

- Always read the actual source code at the error location, don't guess
- Check recent changes (git diff, git log) — the bug is likely in new code
- Reproduce before fixing — understand the trigger condition
- Fix the root cause, not the symptom
- One fix at a time — don't change multiple things simultaneously
