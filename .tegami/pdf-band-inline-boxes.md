---
packages:
  takumi-pdf: minor
  takumi-core: minor
---

# `renderWithBoxes` reports header and footer bands, fixed boxes, and inline elements

Each box now says which tree it is in with `origin`, `"content"`, `"header"`, or `"footer"`, and, for a band taken from `pages`, which entry with `rule`. A `position: fixed` box has a box on every page it repeats on, and an inline element such as `<b>` in a paragraph has one per line around its text. In `takumi-core`, `MeasuredInlineRun` gains `line`, `element`, and `visible`, and is now `#[non_exhaustive]`, so code that builds or fully destructures one no longer compiles.
