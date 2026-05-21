---
name: code-review
description: Automated code review — analyzes source files for bugs, security issues, style violations, and improvement suggestions. Use when asked to review code, check quality, or find problems in files.
---

# Code Review

## Overview

Perform thorough code reviews on source files. Identifies bugs, security vulnerabilities, performance issues, code style violations, and suggests improvements.

## When to Use

- User asks to "review", "check", or "audit" code
- Pull request or diff review requests
- Quality assessment before deployment
- Finding bugs or security issues in existing code

## Workflow

1. **Read the target files** using `read_file` to get the source code
2. **Analyze** the code for the categories below
3. **Report findings** organized by severity (Critical > Warning > Info)
4. **Suggest fixes** with concrete code snippets

## Review Categories

### 1. Bugs & Logic Errors
- Off-by-one errors, null/undefined access, race conditions
- Incorrect boolean logic, missing edge cases
- Resource leaks (unclosed files, connections, memory)

### 2. Security
- SQL injection, XSS, command injection
- Hardcoded secrets, insecure crypto
- Path traversal, unsafe deserialization
- Missing input validation at boundaries

### 3. Performance
- N+1 queries, unnecessary allocations
- Missing indexes for database queries
- Blocking calls in async code
- Inefficient algorithms (O(n^2) where O(n) is possible)

### 4. Code Quality
- Dead code, unused variables/imports
- Functions too long (>50 lines), too many parameters (>5)
- Missing error handling, swallowed exceptions
- Inconsistent naming conventions

### 5. Maintainability
- Missing or misleading comments on complex logic
- Tight coupling, violation of single responsibility
- Magic numbers, duplicated logic

## Output Format

For each finding:
```
[SEVERITY] Category — file:line
Description of the issue.
Suggested fix: ...
```

Severity levels:
- **CRITICAL** — Bugs, security holes, data loss risk
- **WARNING** — Performance issues, potential bugs, bad practices
- **INFO** — Style, readability, minor improvements

## Example

```
[CRITICAL] Security — src/api/auth.rs:45
SQL query built with string concatenation. Vulnerable to SQL injection.
Suggested fix: Use parameterized queries instead.

[WARNING] Performance — src/service/fetch.rs:102
Blocking HTTP call inside async function.
Suggested fix: Use reqwest::Client async methods.

[INFO] Code Quality — src/utils/helpers.rs:12
Unused import `std::collections::BTreeMap`.
Suggested fix: Remove the import.
```

## Best Practices

- Review files one at a time for thoroughness
- Start with entry points (main, routes, handlers) then follow call chains
- Always check error handling paths, not just happy paths
- For large codebases, focus on changed files or critical paths first
