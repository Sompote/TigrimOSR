---
name: refactor-bot
description: Suggest and apply code refactoring patterns — simplify complex code, extract functions, reduce duplication, improve naming, and modernize syntax.
---

# Refactor Bot

## Overview

Analyze code for refactoring opportunities and apply improvements. Simplifies complex logic, extracts reusable functions, eliminates duplication, and modernizes patterns.

## When to Use

- User asks to "refactor", "clean up", or "simplify" code
- Code is hard to read or maintain
- Reducing duplication across files
- Modernizing legacy code patterns
- Preparing code for extension or reuse

## Workflow

1. **Read target files** to understand current code structure
2. **Identify refactoring opportunities** from the categories below
3. **Propose changes** with before/after examples
4. **Apply changes** using `write_file`, preserving behavior
5. **Verify** the refactored code maintains the same functionality

## Refactoring Patterns

### Extract Function
When a code block does a distinct task within a larger function:
```
Before: 50-line function with inline validation, processing, formatting
After:  validate_input() + process_data() + format_output()
```

### Remove Duplication
When similar code appears in multiple places:
```
Before: Same 10 lines repeated in 3 handlers with minor variations
After:  Shared helper function called from all 3 handlers
```

### Simplify Conditionals
When nested if/else chains are hard to follow:
```
Before: 4 levels of nested if/else
After:  Early returns + guard clauses, flat structure
```

### Rename for Clarity
When names don't convey meaning:
```
Before: let x = get_d(p, 2);
After:  let distance = calculate_distance(point, dimensions);
```

### Replace Magic Numbers
When literal values lack context:
```
Before: if retries > 3 { sleep(5000); }
After:  const MAX_RETRIES: u32 = 3; const RETRY_DELAY_MS: u64 = 5000;
```

### Modernize Syntax
Language-specific modernizations:
- **Rust:** Use `?` instead of match/unwrap chains, iterators instead of manual loops
- **Python:** f-strings, walrus operator, dataclasses, pathlib
- **JS/TS:** async/await over callbacks, optional chaining, nullish coalescing

### Reduce Nesting
```
Before:
if condition {
    if other_condition {
        // actual work
    }
}

After:
if !condition { return; }
if !other_condition { return; }
// actual work
```

## Rules

- **Preserve behavior** — refactoring must not change what the code does
- **One pattern at a time** — don't mix multiple refactoring types in one change
- **Keep it minimal** — don't refactor code that's already clear
- **Don't over-abstract** — 3 similar lines are better than a premature abstraction
- **Respect project conventions** — match existing style and patterns

## Output Format

For each refactoring:
```
## [Pattern Name] — file:line_range

**Why:** Brief explanation of the problem
**Before:** Original code snippet
**After:** Refactored code snippet
**Impact:** What improves (readability, maintainability, performance)
```
