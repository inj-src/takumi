---
packages:
  takumi-pdf: minor
---

# Report where each element lands on the pages with `renderWithBoxes`

`renderWithBoxes` renders like `render` and also returns a box for every fragment of every input node on the pages: its page, its source order in the input tree, its tag name, and its rect in pt from the page's top-left corner. Rust callers use `render_with_boxes`.
