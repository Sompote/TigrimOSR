---
name: product-research-pipeline
description: Combined workflow for product/brand research that integrates typo handling, web search, data fetching, and report generation
---

# Product Research Pipeline Skill

## Overview
This skill provides an end-to-end workflow for researching products or brands, handling typos automatically, searching the web, gathering detailed data, and compiling a structured report.


## Workflow

### Phase 1: Brand Discovery
1. Accept user query (may contain typos)
2. Generate spelling variations
3. Search iteratively until valid results found
4. Confirm correct brand name

### Phase 2: Data Collection
1. Search for general brand/product information
2. Fetch official website for product details
3. Search for market presence and competitors
4. Gather target audience and pricing info

### Phase 3: Report Compilation
1. Structure findings into markdown report
2. Create supporting visualizations (charts, tables)
3. Generate executive summary
4. Output complete research document

## Implementation

```python
def product_research_pipeline(query: str) -> str:
    """End-to-end product research workflow."""
    
    # Phase 1: Discovery
    correct_name, search_results = brand_discovery_with_typos(query)
    
    if not search_results:
        return f"Could not find information about '{query}'. Please verify the brand name."
    
    # Phase 2: Data Collection
    product_info = search_product_details(correct_name)
    website_data = fetch_official_website(correct_name)
    competitors = search_competitors(correct_name)
    
    # Phase 3: Report
    report = compile_research_report(
        product_name=correct_name,
        search_results=search_results,
        product_info=product_info,
        website_data=website_data,
        competitors=competitors
    )
    
    return report

def compile_research_report(product_name, search_results, product_info, 
                           website_data, competitors) -> str:
    """Compile all data into structured report."""
    return f"""# 📊 {product_name} Marketing Research Report

## Executive Summary
[Brief overview of key findings]

## Product Overview
| Aspect | Details |
|--------|---------|
| Product Name | {product_info.get('name', product_name)} |
| Product Type | {product_info.get('type', 'N/A')} |
| Website | {product_info.get('website', 'N/A')} |

## Market Presence
{search_results['summary']}

## Competitive Landscape
{competitors}

## Recommendations
[Based on findings]
"""
```

## Best Practices
- Handle typos BEFORE searching to avoid wasted API calls
- Always fetch official website for accurate product details
- Include competitor analysis for market context
- Create visualizations (bar charts) for quantitative data
- Use emoji headers for visual hierarchy
- Set a maximum of 3 web search iterations before asking user for clarification


## Error Handling
- If brand not found after all variations: Ask user for clarification
- If website fetch fails: Continue with search results only
- If competitors not found: Note "limited competition detected"
