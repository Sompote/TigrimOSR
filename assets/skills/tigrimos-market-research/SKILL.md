---
name: tigrimos-market-research
description: Multi-agent market research workflow for TigrimOS with brand discovery, data analysis, charts, and compiled report
---

# TigrimOS Market Research Pipeline

## Overview
This skill orchestrates a multi-agent workflow to perform comprehensive market research for TigrimOS (or similar AI assistant/agent platforms). It handles brand name correction, market data collection, visualization generation, and report compilation.

## Multi-Agent Topology

```
[Orchestrator]
    ├── spawn: brand_discovery_agent (sequential, waits)
    ├── spawn: market_researcher (parallel)
    ├── spawn: data_analyst (parallel)
    ├── spawn: report_compiler (parallel, waits for analyst)
    └── synthesize: all outputs → final report
```

### Agent Roles

| Agent | Label | Role | Waits For |
|-------|-------|------|----------|
| Orchestrator | orchestrator | Coordinates workflow, synthesizes results | - |
| Brand Discovery | brand_discovery_agent | Typos/variation handling | orchestrator |
| Market Researcher | market_researcher | Web search, data gathering | brand_discovery |
| Data Analyst | data_analyst | Charts, visualizations, JSON data | market_researcher |
| Report Compiler | report_compiler | Markdown report assembly | market_researcher, data_analyst |

### Skills Loaded Per Agent

- **orchestrator**: No specific skill load
- **brand_discovery_agent**: Load `brand-discovery` skill
- **market_researcher**: No specific skill load
- **data_analyst**: Load `language-ranking-chart` skill
- **report_compiler**: No specific skill load

## Workflow

### Phase 1: Brand Discovery
1. Accept user query (may contain typos)
2. Load `brand-discovery` skill
3. Generate spelling variations and search iteratively
4. Confirm correct brand name (e.g., "TigrimOS")
5. Pass verified name to downstream agents

### Phase 2: Market Research (Parallel Agents)
1. **Market Researcher**: Search web for:
   - Market size and growth projections
   - Competitive landscape (OpenAI, Anthropic, Google, Microsoft)
   - Target audience segments
   - SWOT analysis
2. **Data Analyst**: Generate visualizations:
   - Market growth chart (line/bar)
   - Competitive landscape chart (bar)
   - Target audience distribution (pie)
   - SWOT analysis diagram
   - Save as PNG files with JSON data backup

### Phase 3: Report Compilation
1. Load market research data
2. Load chart outputs
3. Generate comprehensive markdown report:
   - Executive summary
   - Market overview with tables
   - Charts and visualizations
   - Strategic recommendations
4. Save to output directory

## Implementation

```python
def tigrimos_market_research_pipeline(query: str) -> dict:
    """Execute multi-agent market research workflow."""
    
    # Phase 1: Brand Discovery
    brand_agent = spawn_agent("brand_discovery_agent", 
                              skills=["brand-discovery"])
    correct_name = brand_agent.discover(query)  # Handles typos
    
    # Phase 2: Parallel Agents
    market_researcher = spawn_agent("market_researcher")
    data_analyst = spawn_agent("data_analyst", 
                                skills=["language-ranking-chart"])
    
    # Execute in parallel
    market_data = market_researcher.research(correct_name)
    charts = data_analyst.create_charts(market_data)
    
    # Phase 3: Report Compilation
    report_compiler = spawn_agent("report_compiler")
    final_report = report_compiler.compile(
        product_name=correct_name,
        market_data=market_data,
        charts=charts
    )
    
    return {
        "product_name": correct_name,
        "market_data": market_data,
        "charts": charts,
        "report": final_report
    }
```

## Expected Outputs

| Output | File | Agent |
|--------|------|-------|
| Market research data | `*_market_research_report.md` | market_researcher |
| Growth chart | `market_growth_chart.png` | data_analyst |
| Competitive landscape | `competitive_landscape.png` | data_analyst |
| Target audience | `target_audience.png` | data_analyst |
| SWOT analysis | `swot_analysis.png` | data_analyst |
| Final report | `*_Final_Report.md` | report_compiler |

## Best Practices
- Always run brand discovery FIRST to handle typos ("tigrimos" → "TigrimOS")
- Run market research and data analysis in parallel for efficiency
- Data analyst should wait for market researcher results before creating charts
- Report compiler should wait for both research and charts
- Save all charts as PNG + JSON for downstream use
- Use descriptive filenames with product name prefix

## Error Handling
- If brand discovery finds no match: Ask user to verify spelling
- If market researcher times out: Retry once, then fall back to direct web search
- If data analyst times out: Skip visualizations, continue with text-only report
- If report compiler times out: Use orchestrator to directly synthesize results

## Output Directory Structure
```
./sandbox/output_file/
├── TigrimOS_Marketing_Research_Report.md
├── market_growth_chart.png
├── competitive_landscape.png
├── target_audience.png
├── swot_analysis.png
└── [data_analyst_outputs.json]
```
