---
name: doc-generator
description: Generate documentation from source code — creates README, API docs, module summaries, and inline documentation. Use when asked to document code, generate docs, or explain a codebase.
---

# Documentation Generator

## Overview

Analyze source code and generate comprehensive documentation including README files, API references, module overviews, and usage examples.

## When to Use

- User asks to "document", "write docs", or "generate documentation"
- Creating README for a project or module
- API documentation from function signatures
- Explaining how a codebase works

## Workflow

1. **Scan the project** using `list_files` to understand structure
2. **Read key files** — entry points, public APIs, config files
3. **Analyze** code structure, exports, function signatures, types
4. **Generate** documentation in the requested format
5. **Write** output using `write_file`

## Documentation Types

### 1. Project README
Generate a complete README.md with:
- Project name and description
- Installation instructions (from package.json, Cargo.toml, requirements.txt, etc.)
- Usage examples
- Configuration options
- Project structure overview
- Contributing guidelines

### 2. API Reference
For each public function/method/endpoint:
- Function signature with types
- Parameter descriptions
- Return value description
- Usage example
- Error cases

### 3. Module Summary
For each module/file:
- Purpose and responsibility
- Key exports (functions, types, constants)
- Dependencies (what it imports)
- Usage patterns

### 4. Inline Documentation
Add documentation comments to existing code:
- Function/method docstrings
- Type/struct descriptions
- Complex logic explanations
- Module-level overview comments

## Output Format

Use Markdown by default. Structure:
```markdown
# Project Name

Brief description.

## Installation
...

## Usage
...

## API Reference

### `function_name(param1: Type, param2: Type) -> ReturnType`
Description of what it does.

**Parameters:**
- `param1` — description
- `param2` — description

**Returns:** description

**Example:**
...
```

## Best Practices

- Read actual code, don't guess at functionality
- Include concrete examples derived from real usage in the code
- Document error cases and edge cases, not just happy paths
- Keep descriptions concise — one sentence for simple items
- Match the language conventions (rustdoc for Rust, JSDoc for JS, docstrings for Python)
- Organize by logical grouping, not file order
