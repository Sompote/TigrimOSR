---
name: python-bar-chart-generator
description: Create bar charts from data using Python with matplotlib and compile results into visualization reports
---

# Python Bar Chart Generator Skill

## Overview
This skill creates bar charts from data using Python (matplotlib/seaborn) and saves visualizations as PNG files.

## Workflow
1. **Prepare Data** - Structure data as lists or dictionaries
2. **Create Chart** - Use matplotlib to generate bar chart
3. **Style Chart** - Apply colors, labels, and formatting
4. **Save Output** - Export as PNG file
5. **Display Results** - Show chart and summary table

## Implementation

```python
import matplotlib.pyplot as plt

def create_bar_chart(data: dict, title: str, xlabel: str, ylabel: str, filename: str):
    """Create a styled bar chart from dictionary data."""
    plt.figure(figsize=(10, 6))
    plt.bar(data.keys(), data.values(), color='steelblue')
    plt.title(title, fontsize=14, fontweight='bold')
    plt.xlabel(xlabel)
    plt.ylabel(ylabel)
    plt.xticks(rotation=45)
    plt.tight_layout()
    plt.savefig(filename)
    plt.close()
    return filename
```

## Usage Example

```python
# Programming languages data
languages = {
    'Python': 15.24,
    'C': 11.48,
    'C++': 10.65,
    'Java': 10.42,
    'C#': 7.15
}

create_bar_chart(languages, 'Top 5 Programming Languages 2024', 'Language', 'Rating (%)', 'chart.png')
```

## Best Practices
- Use tight_layout() to prevent label cutoff
- Choose contrasting colors for readability
- Add value labels on bars when showing specific numbers
- Include source attribution in chart title
- Save high-resolution PNG (dpi=300)
