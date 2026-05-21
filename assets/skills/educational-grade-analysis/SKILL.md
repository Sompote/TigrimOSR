---
name: educational-grade-analysis
description: Excel-based student grade analysis with percentage conversion, distribution charts, and detailed performance breakdown
---

# Educational Grade Analysis Skill

## Overview
This skill analyzes student exam/assignment scores from Excel files, converts raw scores to percentages, generates distribution visualizations, and provides detailed performance analysis by question.

## Workflow

### Phase 1: Data Loading
1. Read Excel file (`.xlsx`) using pandas
2. Identify score columns and total possible points
3. Handle missing values appropriately

### Phase 2: Percentage Conversion
1. Calculate percentage: `(score / total_points) * 100`
2. Handle bonus points (scores can exceed 100%)
3. Apply grade thresholds:
   - **A**: 80-100%
   - **B**: 70-79%
   - **C**: 60-69%
   - **D**: 50-59%
   - **F**: Below 50%

### Phase 3: Statistical Analysis
1. Calculate mean, median, standard deviation
2. Identify highest/lowest scores
3. Compute passing rates at different thresholds

### Phase 4: Visualizations
Generate 4-panel visualization saved as PNG:
1. **Score Distribution Histogram** - Raw percentage scores with mean line
2. **Grade Distribution Bar Chart** - Breakdown by letter grade
3. **Grade Pie Chart** - Percentage share of each grade
4. **Box Plot by Question** - Per-question performance comparison

### Phase 5: Question Analysis
1. Calculate average percentage per question
2. Identify challenging questions (low avg)
3. Provide actionable insights for improvement

## Implementation

```python
import pandas as pd
import matplotlib.pyplot as plt
import numpy as np

def analyze_grades(filepath: str, total_points: float, score_col: str = None) -> dict:
    """Complete grade analysis workflow."""
    
    # Phase 1: Load data
    df = pd.read_excel(filepath)
    if score_col:
        scores = df[score_col]
    else:
        # Assume last column or column with 'score' in name
        scores = df.iloc[:, -1]
    
    # Phase 2: Convert to percentages
    percentages = (scores / total_points) * 100
    df['Percentage'] = percentages
    
    # Assign grades
    def assign_grade(pct):
        if pct >= 80: return 'A'
        elif pct >= 70: return 'B'
        elif pct >= 60: return 'C'
        elif pct >= 50: return 'D'
        else: return 'F'
    
    df['Grade'] = df['Percentage'].apply(assign_grade)
    
    # Phase 3: Statistics
    stats = {
        'mean': df['Percentage'].mean(),
        'std': df['Percentage'].std(),
        'max': df['Percentage'].max(),
        'min': df['Percentage'].min(),
        'passing_rate': (df['Grade'] != 'F').mean() * 100
    }
    
    # Phase 4: Visualizations
    fig, axes = plt.subplots(2, 2, figsize=(14, 10))
    
    # Histogram
    axes[0, 0].hist(df['Percentage'], bins=20, edgecolor='black', alpha=0.7)
    axes[0, 0].axvline(stats['mean'], color='red', linestyle='--', label=f'Mean: {stats["mean"]:.1f}%')
    axes[0, 0].set_title('Score Distribution')
    axes[0, 0].set_xlabel('Percentage')
    axes[0, 0].legend()
    
    # Grade bar
    grade_counts = df['Grade'].value_counts().reindex(['A', 'B', 'C', 'D', 'F'])
    colors = ['#2ecc71', '#3498db', '#f1c40f', '#e67e22', '#e74c3c']
    axes[0, 1].bar(grade_counts.index, grade_counts.values, color=colors)
    axes[0, 1].set_title('Grade Distribution')
    axes[0, 1].set_ylabel('Number of Students')
    
    # Pie chart
    axes[1, 0].pie(grade_counts.values, labels=grade_counts.index, colors=colors, autopct='%1.1f%%')
    axes[1, 0].set_title('Grade Distribution (%)')
    
    plt.tight_layout()
    plt.savefig('grade_analysis.png', dpi=150)
    plt.close()
    
    return {'stats': stats, 'dataframe': df, 'chart_file': 'grade_analysis.png'}
```

## Best Practices
- Always normalize to percentage for fair comparison across exams
- Use TIOBE-style color coding (green=good, red=poor) for grades
- Include mean line on histogram for benchmark reference
- Set consistent grade thresholds or explain deviations
- Handle bonus/extra credit by allowing >100% scores
- Save charts with descriptive filenames including course/exam name

## Output Format
- **Statistics**: Mean, std dev, min/max, passing rate
- **Visualization**: 4-panel PNG chart
- **Data**: Extended dataframe with percentages and grades
- **Analysis**: Question-by-question performance breakdown
