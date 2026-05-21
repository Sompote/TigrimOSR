---
name: git-summarize
description: Summarize git history into changelogs, release notes, or activity reports. Analyzes commits, diffs, and branch history.
---

# Git Summarize

## Overview

Analyze git repository history and generate human-readable summaries — changelogs, release notes, contributor reports, and activity overviews.

## When to Use

- User asks for a changelog or release notes
- Summarizing what changed between versions/branches
- Activity reports (what happened this week/sprint)
- Understanding what a branch or PR changes

## Workflow

1. **Gather git data** using `run_shell` with git commands
2. **Parse commit messages** and categorize changes
3. **Generate summary** in the requested format
4. **Write output** using `write_file` if saving to file

## Git Commands

### Recent Commits
```bash
# Last 20 commits, one line each
git log --oneline -20

# With dates and authors
git log --format="%h %ad %an: %s" --date=short -20

# Since a date
git log --since="2025-01-01" --oneline

# Since a tag
git log v1.0.0..HEAD --oneline
```

### Between Versions
```bash
# Changes between two tags
git log v1.0.0..v2.0.0 --oneline

# Changes between branches
git log main..feature-branch --oneline

# With full diff stats
git diff --stat v1.0.0..v2.0.0
```

### File Changes
```bash
# Files changed in last 10 commits
git diff --name-only HEAD~10..HEAD

# Files changed between versions
git diff --name-status v1.0.0..v2.0.0
```

### Contributors
```bash
# Author summary
git shortlog -sn --since="2025-01-01"

# Commits per author this month
git log --since="1 month ago" --format="%an" | sort | uniq -c | sort -rn
```

## Output Formats

### Changelog (Categorized)
```markdown
# Changelog

## [v2.0.0] - 2025-05-21

### Added
- New PDF preview in output panel
- Chunked PDF reading for agents

### Fixed
- Context overflow when reading large files
- Version display showing old version

### Changed
- Improved error detection for API limits
```

### Release Notes (Narrative)
```markdown
# Release Notes — v2.0.0

This release adds PDF preview support and improves stability when working
with large files.

**Highlights:**
- PDF files now render as visual page previews in the output panel
- Agents can read PDFs in chunks, preventing context overflow
- Better error recovery for API rate limits
```

### Activity Report
```markdown
# Weekly Activity Report (May 15-21, 2025)

**Commits:** 12
**Contributors:** 3
**Files changed:** 28

**Summary:**
- Major: PDF rendering feature added
- Bug fixes: context overflow, version display
- Refactoring: compact.rs error detection
```

## Commit Categorization

Automatically categorize commits by prefix or content:
- `feat:` / `add` / `new` → **Added**
- `fix:` / `bug` / `patch` → **Fixed**
- `refactor:` / `clean` → **Changed**
- `docs:` / `readme` → **Documentation**
- `test:` / `spec` → **Testing**
- `chore:` / `ci:` / `build:` → **Maintenance**
- `perf:` / `optimize` → **Performance**
- `BREAKING` → **Breaking Changes**

## Best Practices

- Always read actual commit messages, don't fabricate
- Group related commits into single changelog entries
- Highlight breaking changes prominently
- Include commit hashes for traceability
- For release notes, focus on user impact not implementation details
