---
name: file-search
description: Fast recursive file search — find files by name, extension, content, or size. Use for locating files in large codebases, finding references, or discovering project structure.
---

# File Search

## Overview

Search for files and content across directories using shell commands. Find files by name patterns, search inside files for text/regex, and analyze project structure.

## When to Use

- User asks to "find", "search for", or "locate" files
- Looking for where a function, class, or variable is defined
- Finding all files of a certain type
- Searching for specific text across a codebase
- Understanding project structure

## Search Commands

### Find Files by Name

```bash
# Find by exact name
find . -name "config.json" -type f

# Find by extension
find . -name "*.rs" -type f

# Find by pattern (case-insensitive)
find . -iname "*test*" -type f

# Find in specific directory
find src/ -name "*.py" -type f
```

### Search File Contents

```bash
# Search for text in all files
grep -rn "function_name" .

# Search with file type filter
grep -rn --include="*.rs" "struct Config" .

# Search case-insensitive
grep -rni "todo\|fixme\|hack" .

# Search with context (3 lines before/after)
grep -rn -C 3 "error_handler" src/

# Search for regex pattern
grep -rn -E "fn\s+\w+\(" src/
```

### Find by Size/Date

```bash
# Files larger than 1MB
find . -type f -size +1M

# Files modified in last 7 days
find . -type f -mtime -7

# Files modified today
find . -type f -mtime 0
```

### Project Structure

```bash
# Directory tree (2 levels deep)
find . -maxdepth 2 -type d | sort

# Count files by extension
find . -type f | sed 's/.*\.//' | sort | uniq -c | sort -rn | head -20

# Find largest files
find . -type f -exec ls -la {} \; | sort -k5 -rn | head -20
```

## Common Use Cases

### Find Where a Function is Defined
```bash
grep -rn "fn function_name\|def function_name\|function function_name" src/
```

### Find All Imports of a Module
```bash
grep -rn "import.*module_name\|from module_name\|use.*module_name\|require.*module_name" .
```

### Find TODO/FIXME Comments
```bash
grep -rn "TODO\|FIXME\|HACK\|XXX\|WARN" --include="*.rs" --include="*.py" --include="*.ts" .
```

### Find Configuration Files
```bash
find . -name "*.json" -o -name "*.yaml" -o -name "*.yml" -o -name "*.toml" -o -name "*.env*" | grep -v node_modules | grep -v target
```

## Best Practices

- Exclude build directories: add `--exclude-dir=node_modules --exclude-dir=target --exclude-dir=.git` to grep
- Use `list_files` tool first for simple directory listings
- Use `read_file` to examine files once found
- Combine find + grep for complex searches
- Use `-l` flag with grep to get just file names (no content)
