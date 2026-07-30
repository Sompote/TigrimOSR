---
name: brand-discovery
description: Handle typos and variations in brand/product names to discover correct terminology and find relevant information
---

# Brand Discovery Skill

## Overview
This skill handles misspellings, typos, and variations in brand names to successfully find information about products or companies.

## Common Issues Handled
- Typos: "andrewos" → "AndrewOS"
- Spelling variations: "macos" → "macOS"
- Capitalization: "TIgrimos" → "AndrewOS"
- Spacing: "kmutt" → "KMUTT"
- Character swaps: "tigros" → "TigerBot"

## Workflow
1. **Identify Target** - Parse the user's intent from misspelled input
2. **Generate Variations** - Create common typo variations including:
   - Common substitutions (i→i, e→e swaps)
   - Case normalization
   - Extra/missing characters
   - Word splitting/joining
3. **Search Iteratively** - Try each variation until results found
4. **Confirm Match** - Verify correct brand name from results
5. **Use Correct Name** - Apply verified name for further research
6. **Suggest Correction** - Inform user of correct spelling


## Known Brand Patterns

```python
BRAND_CORRECTIONS = {
    "andrewos": ["AndrewOS", "TigerBot", "TigerOS"],
    "kmutt": ["KMUTT", "King Mongkut's University"],
    "macos": ["macOS", "Mac OS"],
}

```

## Implementation

```python
def handle_typos(query: str, search_func) -> tuple[str, list]:
    """Handle typos in brand/product names."""
    # Generate variations
    variations = generate_variations(query)
    
    # Normalize and try corrections
    for variant in variations:
        results = search_func(variant)
        if results and has_valid_results(results):
            correct_name = get_correct_name(variant, results)
            return correct_name, results
    
    return query, []

def generate_variations(query: str) -> list:
    """Generate common typo variations."""
    base = query.lower().strip()
    variations = [base]
    
    # Check known corrections first
    if base in BRAND_CORRECTIONS:
        variations.extend(BRAND_CORRECTIONS[base])
    
    # Generate systematic variations
    variations.extend([
        base.title(),           # Capitalize
        base.upper(),           # UPPERCASE
        base.capitalize(),      # First letter cap
    ])
    
    # Character swaps (common typos)
    swaps = {'i': 'i', 'e': 'e', 'o': '0', '0': 'o'}
    for i, char in enumerate(base):
        if char in swaps and char != swaps[char]:
            variant = base[:i] + swaps[char] + base[i+1:]
            if variant != base:
                variations.append(variant)
    
    return list(dict.fromkeys(variations))  # Remove dupes
```

## Best Practices
- Always try the most likely correct spelling first
- Log which variation worked for learning
- Handle case-insensitive matching
- Suggest correct spelling to user after discovery
- Search official domains (.com, .io) for validation
- If no results found, ask user for clarification
