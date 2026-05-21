---
name: language-ranking-chart
description: Create professional bar charts for programming language and software rankings with proper styling and TIOBE index formatting
---

# Language Ranking Chart Skill

## Overview
This skill creates visually appealing bar charts specifically for programming language rankings, software popularity metrics, and similar comparison data.

## Workflow
1. **Fetch Rankings** - Search for current ranking data (TIOBE, Stack Overflow, GitHub)
2. **Validate Data** - Ensure rating percentages sum reasonably
3. **Create Chart** - Use matplotlib with ranking-specific styling
4. **Apply Ranking Theme** - Professional colors with ranking hierarchy emphasis
5. **Save and Display** - Export PNG with embedded summary table

## Implementation

```python
import matplotlib.pyplot as plt

def create_ranking_chart(data: dict, title: str, source: str, filename: str):
    """Create a ranking bar chart with professional styling."""
    plt.figure(figsize=(10, 6))
    
    colors = ['#2ecc71', '#3498db', '#9b59b6', '#f39c12', '#e74c3c'][:len(data)]
    bars = plt.bar(range(len(data)), list(data.values()), color=colors)
    
    plt.title(f'{title}\nSource: {source}', fontsize=14, fontweight='bold')
    plt.xticks(range(len(data)), list(data.keys()), rotation=45)
    plt.ylabel('Rating (%)')
    plt.xlabel('Programming Language')
    
    # Add value labels
    for bar, val in zip(bars, data.values()):
        plt.text(bar.get_x() + bar.get_width()/2, bar.get_height() + 0.2, 
                f'{val}%', ha='center', va='bottom', fontsize=10)
    
    plt.tight_layout()
    plt.savefig(filename, dpi=150)
    plt.close()
    return filename
```

## Best Practices
- Use TIOBE-style green-to-red color gradients for top rankings
- Include source attribution in title
- Add percentage labels on bars for precision
- Limit to top 5-10 items for readability
- Use tight_layout() to prevent label cutoff

## Output Format
Returns PNG file plus summary table with rank, language, and rating columns.
