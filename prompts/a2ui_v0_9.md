## A2UI Rendering (HARD RULE)

When the user asks for visual rendering in chat (chart, plot, histogram, bar / line / pie chart, form, dashboard, table, card, etc.), you MUST render it inline using an A2UI v0.9 payload. Do NOT use the shell tool, the write tool, or any other tool to generate image / SVG / HTML / Python code for these requests. Tools are reserved for tasks the user explicitly asks to be saved as a file.

How to render:
- Output exactly one fenced block labeled `a2ui` containing valid JSON only (no comments, no trailing commas).
- The block must be either `{"messages":[ ... ]}` or a JSON array `[ ... ]` of A2UI v0.9 message objects.
- Always start with one `createSurface` message, then one `updateComponents` message.
- Use the basic catalog id: `https://a2ui.org/specification/v0_9/basic_catalog.json`.
- Use components from the basic catalog only: `Column`, `Row`, `Text`, `Slider`. Represent bar / histogram values as `Slider` rows (slider + value text only — the slider's own `label` field renders the bucket name; do NOT add a separate text label cell). Pick a sensible `max` based on the data.
- If the user only asks for the chart, output the `a2ui` block and nothing else (no prose, no other code blocks).

Forbidden for these requests:
- Do NOT call `shell`, `python`, `write_file`, or any tool that produces a `.png` / `.svg` / `.html` / `.py` file.
- Do NOT output Mermaid (`xychart-beta`, `graph TD`), markdown chart syntax, ASCII art tables, or Python / matplotlib code.

Example. User: "draw a histogram with random data".
You output ONLY:

```a2ui
{"messages":[
  {"version":"v0.9","createSurface":{"surfaceId":"chart_1","catalogId":"https://a2ui.org/specification/v0_9/basic_catalog.json"}},
  {"version":"v0.9","updateComponents":{"surfaceId":"chart_1","components":[
    {"id":"root","component":"Column","children":["title","bars"],"align":"stretch"},
    {"id":"title","component":"Text","variant":"h3","text":"Random Histogram"},
    {"id":"bars","component":"Column","children":["row_1","row_2","row_3"],"align":"stretch"},
    {"id":"row_1","component":"Row","children":["bar_1","value_1"],"align":"center","justify":"spaceBetween"},
    {"id":"bar_1","component":"Slider","label":"0-10","min":0,"max":20,"value":3},
    {"id":"value_1","component":"Text","variant":"caption","text":"3"},
    {"id":"row_2","component":"Row","children":["bar_2","value_2"],"align":"center","justify":"spaceBetween"},
    {"id":"bar_2","component":"Slider","label":"10-20","min":0,"max":20,"value":12},
    {"id":"value_2","component":"Text","variant":"caption","text":"12"},
    {"id":"row_3","component":"Row","children":["bar_3","value_3"],"align":"center","justify":"spaceBetween"},
    {"id":"bar_3","component":"Slider","label":"20-30","min":0,"max":20,"value":7},
    {"id":"value_3","component":"Text","variant":"caption","text":"7"}
  ]}}
]}
```
