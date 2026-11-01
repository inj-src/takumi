//! Where each element lands on the pages, for tools that map a page back to its source.

use std::{
  collections::{BTreeMap, HashMap, HashSet},
  iter::{repeat_n, successors},
  ptr,
};

use takumi_core::{
  font_style::SizedFontStyle,
  geometry::{Point as CorePoint, Size},
  layout::{
    node::{Node, NodeKind},
    tree::RenderNode,
  },
  paint_chunk::{ChunkPart, PaintChunk},
  paint_property::{ClipId, ClipKind, ClipNode},
  painter::OwnContent,
  scene::{NodePaint, SceneView},
  style::Display,
};

use crate::{
  emitter::shape_extents,
  interactive::{PaintedBox, page_rect, transformed_rect},
  krilla::geom::Rect as KrillaRect,
  options::PageRule,
  tree::INPUT_ROOT,
};

/// One fragment of an element on one page, in pt from the page's top-left corner.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct PageBox {
  /// The output page, from 0.
  pub page: usize,
  /// The tree the element is in.
  pub origin: BoxOrigin,
  /// The [`PageRules`](crate::PageRules) entry the page took its band from, `None` outside one.
  pub rule: Option<PageRule>,
  /// Its position in its tree, every node counted in document order from 0 at the root.
  pub source_order: usize,
  /// The element's tag name, absent for a node built without one, such as text.
  pub tag_name: Option<String>,
  /// The left edge.
  pub x: f32,
  /// The top edge.
  pub y: f32,
  /// The width.
  pub width: f32,
  /// The height.
  pub height: f32,
}

/// The tree a [`PageBox`]'s element is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BoxOrigin {
  /// The document's node, its repeated `position: fixed` boxes included.
  Content,
  /// A header band.
  Header,
  /// A footer band.
  Footer,
}

/// How the source orders of a tree a page draws lead back to the caller's tree.
pub(crate) struct BoxTree<'o> {
  pub(crate) origin: BoxOrigin,
  pub(crate) rule: Option<PageRule>,
  /// Present when filling in the page's counters changed the tree.
  pub(crate) counters: Option<&'o SourceOrders>,
  /// Where a repeated box laid out again on its own, its root counted from 0, sits in the content.
  pub(crate) subtree: Option<usize>,
}

impl BoxTree<'_> {
  /// The content tree.
  pub(crate) const CONTENT: BoxTree<'static> = BoxTree {
    origin: BoxOrigin::Content,
    rule: None,
    counters: None,
    subtree: None,
  };
}

/// The boxes a render collects as its pages draw.
#[derive(Default)]
pub(crate) struct BoxCollector {
  pub(crate) boxes: Vec<PageBox>,
  /// Present when filling in counters changed the content tree that was laid out.
  pub(crate) orders: Option<SourceOrders>,
}

/// Where a scene draws on a page.
#[derive(Clone, Copy)]
pub(crate) struct PagePlacement {
  /// The output page.
  pub(crate) page: usize,
  /// The area the page shows, in the scene's coordinates.
  pub(crate) window: KrillaRect,
  /// What maps the scene's coordinates to the page's, past the window's top.
  pub(crate) offset: CorePoint<f32>,
}

impl BoxCollector {
  /// Adds the boxes `scene`, laid out from `tree`, leaves visible on the page, in paint order.
  pub(crate) fn collect(&mut self, scene: SceneView<'_>, tree: &BoxTree<'_>, at: PagePlacement) {
    let mut seen = HashSet::new();

    // A box's inline elements paint with its content, which can come after its descendants.
    for chunk in PaintChunk::in_paint_order(scene.contexts) {
      let paint = chunk.node;

      if seen.insert(ptr::from_ref(paint)) {
        self.collect_own(scene, tree, at, paint);
      }
      if chunk.part == ChunkPart::Content {
        self.collect_inline(scene, tree, at, paint);
      }
    }
  }

  fn collect_own(
    &mut self,
    scene: SceneView<'_>,
    tree: &BoxTree<'_>,
    at: PagePlacement,
    paint: &NodePaint,
  ) {
    // A repeated box laid out on its own shares source order 0 with the page area around it.
    if tree.subtree.is_some() && paint.path.is_empty() {
      return;
    }
    let Some(painted) = PaintedBox::of(scene, paint) else {
      return;
    };
    let clip = paint.properties.border_box.clip;

    if let Some(page_box) = self.page_box(scene, tree, at, painted.node, painted.rect, clip) {
      self.boxes.push(page_box);
    }
  }

  /// Adds a box per line for each inline element `paint`'s formatting context holds, around the
  /// text it paints there, outer elements first.
  fn collect_inline(
    &mut self,
    scene: SceneView<'_>,
    tree: &BoxTree<'_>,
    at: PagePlacement,
    paint: &NodePaint,
  ) {
    let Some(node) = scene.root.node_at_path(&paint.path) else {
      return;
    };
    if !node.should_create_inline_layout() {
      return;
    }
    let Ok(layout) = scene.enter(paint) else {
      return;
    };
    let font_style = SizedFontStyle::from_style(&node.context.style, &node.context);
    let Some(items) = OwnContent::of(node).fragment_items(&font_style, layout) else {
      return;
    };
    let (runs, _) = items.measure_runs(node);

    if !runs.iter().any(|run| run.visible && run.element.is_some()) {
      return;
    }
    let elements = InlineElements::of(node);
    let mut lines: BTreeMap<(usize, usize), (f32, f32, f32, f32)> = BTreeMap::new();

    for run in runs.iter().filter(|run| run.visible) {
      for element in elements.enclosing(run.element) {
        let (left, top, right, bottom) = lines.entry((element, run.line)).or_insert((
          f32::INFINITY,
          f32::INFINITY,
          f32::NEG_INFINITY,
          f32::NEG_INFINITY,
        ));

        *left = left.min(run.x);
        *top = top.min(run.y);
        *right = right.max(run.x + run.width);
        *bottom = bottom.max(run.y + run.height);
      }
    }
    // The runs sit in the content box.
    let content = layout.content_box_offset();

    for ((element, _), (left, top, right, bottom)) in lines {
      let Some(rect) = transformed_rect(
        paint.transform,
        CorePoint {
          x: content.x + left,
          y: content.y + top,
        },
        Size {
          width: right - left,
          height: bottom - top,
        },
      ) else {
        continue;
      };
      let Some(element) = elements.node(element) else {
        continue;
      };

      if let Some(page_box) = self.page_box(
        scene,
        tree,
        at,
        element,
        rect,
        paint.properties.contents.clip,
      ) {
        self.boxes.push(page_box);
      }
    }
  }

  /// The visible part of `node`'s `rect`, which paints under `clip`.
  fn page_box(
    &self,
    scene: SceneView<'_>,
    tree: &BoxTree<'_>,
    at: PagePlacement,
    node: &RenderNode,
    rect: KrillaRect,
    clip: Option<ClipId>,
  ) -> Option<PageBox> {
    let source_order = self.input_order(tree, node.source_order()?)?;
    let visible = visible_rect(scene, clip, rect, at.window)?;
    let rect = page_rect(visible, (at.window.top(), at.window.bottom()), at.offset)?;

    Some(PageBox {
      page: at.page,
      origin: tree.origin,
      rule: tree.rule,
      source_order,
      tag_name: node.node.as_ref()?.tag_name().map(str::to_owned),
      x: rect.left(),
      y: rect.top(),
      width: rect.width(),
      height: rect.height(),
    })
  }

  /// The source order in the caller's tree of a node `tree` numbers `laid_out`.
  fn input_order(&self, tree: &BoxTree<'_>, laid_out: usize) -> Option<usize> {
    let laid_out = match tree.subtree {
      Some(_) => laid_out,
      None => laid_out.checked_sub(INPUT_ROOT)?,
    };
    let order = match tree.counters {
      Some(counters) => counters.input_order(laid_out)?,
      None => laid_out,
    };
    if tree.origin != BoxOrigin::Content {
      return Some(order);
    }
    let order = tree.subtree.map_or(order, |start| start + order);

    match &self.orders {
      Some(orders) => orders.input_order(order),
      None => Some(order),
    }
  }
}

/// The inline elements of a formatting context, each with the one around it.
struct InlineElements<'n> {
  /// By source order: the element, and the source order of the inline element around it.
  nodes: HashMap<usize, (&'n RenderNode, Option<usize>)>,
}

impl<'n> InlineElements<'n> {
  fn of(root: &'n RenderNode) -> Self {
    fn walk<'n>(
      node: &'n RenderNode,
      parent: Option<usize>,
      nodes: &mut HashMap<usize, (&'n RenderNode, Option<usize>)>,
    ) {
      for child in node.children.as_deref().unwrap_or_default() {
        if child.context.style.base_data.display != Display::Inline {
          continue;
        }
        let order = child.source_order();

        if let Some(order) = order {
          nodes.insert(order, (child, parent));
        }
        walk(child, order.or(parent), nodes);
      }
    }
    let mut nodes = HashMap::new();

    walk(root, None, &mut nodes);
    Self { nodes }
  }

  /// `element` and the inline elements around it, innermost first.
  fn enclosing(&self, element: Option<usize>) -> impl Iterator<Item = usize> + '_ {
    successors(element, |order| self.nodes.get(order)?.1)
      .filter(|order| self.nodes.contains_key(order))
  }

  fn node(&self, order: usize) -> Option<&'n RenderNode> {
    self.nodes.get(&order).map(|(node, _)| *node)
  }
}

/// The part of `rect` the clips from `clip` outward and the page `window` leave visible.
fn visible_rect(
  scene: SceneView<'_>,
  clip: Option<ClipId>,
  rect: KrillaRect,
  window: KrillaRect,
) -> Option<KrillaRect> {
  successors(clip, |id| scene.properties.clip(*id).parent)
    .filter_map(|id| clip_rect(scene.properties.clip(id)))
    .try_fold(intersect(rect, window)?, intersect)
}

/// The rect a clip's shape lies within in content coordinates, `None` when it narrows nothing.
fn clip_rect(clip: &ClipNode) -> Option<KrillaRect> {
  let (ClipKind::Shape(shape) | ClipKind::ClipPath(shape)) = &clip.kind else {
    return None;
  };
  let (left, top, right, bottom) = shape_extents(shape)?;

  transformed_rect(
    clip.transform,
    CorePoint { x: left, y: top },
    Size {
      width: right - left,
      height: bottom - top,
    },
  )
}

fn intersect(a: KrillaRect, b: KrillaRect) -> Option<KrillaRect> {
  let rect = KrillaRect::from_ltrb(
    a.left().max(b.left()),
    a.top().max(b.top()),
    a.right().min(b.right()),
    a.bottom().min(b.bottom()),
  )?;

  (rect.width() > 0.0 && rect.height() > 0.0).then_some(rect)
}

/// Each laid-out node's place in the caller's tree, once filling in counters changed the tree.
pub(crate) struct SourceOrders(Vec<Option<usize>>);

impl SourceOrders {
  /// Pairs `laid_out` with `input`, a container whose children changed pairing alone.
  pub(crate) fn between(input: &Node, laid_out: &Node) -> Self {
    let mut orders = Vec::new();

    pair_nodes(input, laid_out, &mut 0, &mut orders);
    Self(orders)
  }

  fn input_order(&self, laid_out: usize) -> Option<usize> {
    self.0.get(laid_out).copied().flatten()
  }
}

fn pair_nodes(input: &Node, laid_out: &Node, cursor: &mut usize, orders: &mut Vec<Option<usize>>) {
  orders.push(Some(*cursor));
  *cursor += 1;
  match (&input.kind, &laid_out.kind) {
    (NodeKind::Container { children: before }, NodeKind::Container { children: after })
      if before.len() == after.len() =>
    {
      for (before, after) in before.iter().zip(after) {
        pair_nodes(before, after, cursor, orders);
      }
    }
    _ => {
      *cursor += subtree_len(input) - 1;
      orders.extend(repeat_n(None, subtree_len(laid_out) - 1));
    }
  }
}

fn subtree_len(node: &Node) -> usize {
  match &node.kind {
    NodeKind::Container { children } => 1 + children.iter().map(subtree_len).sum::<usize>(),
    _ => 1,
  }
}

#[cfg(test)]
mod tests {
  use takumi_core::{
    Fonts,
    html::{FromHtmlOptions, from_html},
    layout::node::{Node, NodeKind},
    viewport::Viewport,
  };

  use crate::{
    BoxOrigin, PageBox, PageMargin, PageMargins, PageOptions, PageOverride, PageRule, PageRules,
    PdfOptions, RenderedPdf, options::PT_PER_PX, render, render_with_boxes, test_support::fonts,
  };

  const MARGIN: f32 = 24.0;
  const PAGE: PageOptions = PageOptions {
    width: 320.0,
    height: 240.0,
    margin: PageMargins::uniform(MARGIN),
  };
  const ROWS: usize = 30;

  fn document() -> Node {
    let rows: String = (1..=ROWS)
      .map(|row| format!("<tr><td>Row {row}</td></tr>"))
      .collect();

    from_html(
      &format!(
        r#"<div style="display: flex; flex-direction: column; font-size: 14px;"><h1 style="margin: 0; font-size: 18px;">Title</h1><table style="width: 100%;"><thead><tr><th>Name</th></tr></thead><tbody>{rows}</tbody></table></div>"#
      ),
      FromHtmlOptions::default(),
    )
    .expect("parse the document")
  }

  fn paged(fonts: &Fonts) -> PdfOptions<'_> {
    PdfOptions::builder()
      .node(document())
      .page(PAGE)
      .producer("takumi-pdf test".to_string())
      .fonts(fonts)
      .build()
  }

  fn boxes_of<'b>(rendered: &'b RenderedPdf, tag: &str) -> Vec<&'b PageBox> {
    rendered
      .boxes
      .iter()
      .filter(|page_box| page_box.tag_name.as_deref() == Some(tag))
      .collect()
  }

  fn preorder_index(node: &Node, tag: &str, next: &mut usize) -> Option<usize> {
    let index = *next;

    *next += 1;
    if node.tag_name() == Some(tag) {
      return Some(index);
    }
    let NodeKind::Container { children } = &node.kind else {
      return None;
    };

    children
      .iter()
      .find_map(|child| preorder_index(child, tag, next))
  }

  #[test]
  fn reporting_boxes_leaves_the_pdf_unchanged() {
    let fonts = fonts();
    let rendered = render_with_boxes(paged(&fonts)).expect("render with boxes");

    assert_eq!(rendered.pdf, render(paged(&fonts)).expect("render"));
  }

  #[test]
  fn a_split_element_has_a_box_on_each_page() {
    let fonts = fonts();
    let rendered = render_with_boxes(paged(&fonts)).expect("render with boxes");
    let tables = boxes_of(&rendered, "table");

    assert!(tables.len() > 1, "the table spans pages: {tables:?}");
    assert!(
      tables
        .iter()
        .enumerate()
        .all(|(page, table)| table.page == page && table.source_order == tables[0].source_order)
    );
  }

  #[test]
  fn a_repeated_table_header_is_boxed_on_every_page() {
    let fonts = fonts();
    let rendered = render_with_boxes(paged(&fonts)).expect("render with boxes");
    let pages = boxes_of(&rendered, "table").len();
    let headers = boxes_of(&rendered, "th");

    assert_eq!(headers.len(), pages);
    assert!(
      headers
        .iter()
        .all(|header| header.source_order == headers[0].source_order)
    );
  }

  #[test]
  fn boxes_sit_inside_the_page_area() {
    let fonts = fonts();
    let rendered = render_with_boxes(paged(&fonts)).expect("render with boxes");
    let (left, top) = (MARGIN * PT_PER_PX, MARGIN * PT_PER_PX);
    let (right, bottom) = (
      (PAGE.width - MARGIN) * PT_PER_PX,
      (PAGE.height - MARGIN) * PT_PER_PX,
    );

    for page_box in &rendered.boxes {
      assert!(
        page_box.x >= left - 0.01
          && page_box.y >= top - 0.01
          && page_box.x + page_box.width <= right + 0.01
          && page_box.y + page_box.height <= bottom + 0.01,
        "{page_box:?} leaves the page area"
      );
    }
  }

  #[test]
  fn source_order_counts_every_input_node() {
    let fonts = fonts();
    let rendered = render_with_boxes(paged(&fonts)).expect("render with boxes");
    let heading = boxes_of(&rendered, "h1");

    assert_eq!(
      heading.first().map(|heading| heading.source_order),
      preorder_index(&document(), "h1", &mut 0)
    );
  }

  fn section_order(fonts: &Fonts, counter: &str) -> (Option<usize>, Option<usize>) {
    let source = format!(
      r#"<div style="display: flex; flex-direction: column; font-size: 14px;">{counter}<section>After</section></div>"#
    );
    let node = from_html(&source, FromHtmlOptions::default()).expect("parse the document");
    let expected = preorder_index(&node, "section", &mut 0);
    let rendered = render_with_boxes(
      PdfOptions::builder()
        .node(node)
        .page(PAGE)
        .producer("takumi-pdf test".to_string())
        .fonts(fonts)
        .build(),
    )
    .expect("render with boxes");

    (
      boxes_of(&rendered, "section")
        .first()
        .map(|section| section.source_order),
      expected,
    )
  }

  #[test]
  fn filling_in_counters_keeps_the_input_source_order() {
    let fonts = fonts();

    for counter in [
      r#"<span class="totalPages"></span>"#,
      r#"<span class="pageNumber"><b>0</b><i>0</i></span>"#,
      r##"<a class="targetPageNumber" href="#missing"><b>0</b></a>"##,
    ] {
      let (actual, expected) = section_order(&fonts, counter);

      assert!(expected.is_some());
      assert_eq!(actual, expected, "after {counter}");
    }
  }

  #[test]
  fn a_box_clipped_out_of_sight_is_not_reported() {
    let fonts = fonts();
    let rendered = render_with_boxes(
      PdfOptions::builder()
        .node(
          from_html(
            r#"<div style="display: flex; flex-direction: column;"><div style="width: 40px; height: 40px; overflow: hidden;"><article style="width: 30px; height: 30px; transform: translateY(120px);"></article></div><aside style="width: 30px; height: 30px; transform: translateX(400px);"></aside><section style="height: 250px;"></section></div>"#,
            FromHtmlOptions::default(),
          )
          .expect("parse the document"),
        )
        .page(PAGE)
        .producer("takumi-pdf test".to_string())
        .fonts(&fonts)
        .build(),
    )
    .expect("render with boxes");

    assert!(boxes_of(&rendered, "article").is_empty());
    assert!(boxes_of(&rendered, "aside").is_empty());
    assert!(!boxes_of(&rendered, "section").is_empty());
  }

  #[test]
  fn a_viewport_render_boxes_its_one_page() {
    let fonts = fonts();
    let rendered = render_with_boxes(
      PdfOptions::builder()
        .node(document())
        .viewport(Viewport::new((600, 300)))
        .producer("takumi-pdf test".to_string())
        .fonts(&fonts)
        .build(),
    )
    .expect("render with boxes");

    assert!(!rendered.boxes.is_empty());
    assert!(rendered.boxes.iter().all(|page_box| page_box.page == 0));
    assert_eq!(boxes_of(&rendered, "h1").len(), 1);
  }
  fn html(source: &str) -> Node {
    from_html(source, FromHtmlOptions::default()).expect("parse the document")
  }

  fn rows() -> String {
    (1..=ROWS)
      .map(|row| format!("<tr><td>Row {row}</td></tr>"))
      .collect()
  }

  fn render_paged(fonts: &Fonts, node: Node) -> RenderedPdf {
    render_with_boxes(
      PdfOptions::builder()
        .node(node)
        .page(PAGE)
        .producer("takumi-pdf test".to_string())
        .fonts(fonts)
        .build(),
    )
    .expect("render with boxes")
  }

  fn page_count(rendered: &RenderedPdf) -> usize {
    rendered
      .boxes
      .iter()
      .map(|page_box| page_box.page + 1)
      .max()
      .unwrap_or_default()
  }

  #[test]
  fn bands_are_boxed_in_their_own_trees_on_every_page() {
    let fonts = fonts();
    let header = html(
      r#"<header style="font-size: 10px;"><span class="pageNumber"></span> <b>Invoice</b></header>"#,
    );
    let cover = html(r#"<header style="font-size: 10px;"><strong>Cover</strong></header>"#);
    let footer = html(r#"<footer style="font-size: 10px;"><i>Thanks</i></footer>"#);
    let (b, strong, i) = (
      preorder_index(&header, "b", &mut 0),
      preorder_index(&cover, "strong", &mut 0),
      preorder_index(&footer, "i", &mut 0),
    );
    let rendered = render_with_boxes(
      PdfOptions::builder()
        .node(document())
        .page(PageOptions {
          margin: PageMargins {
            top: PageMargin::Auto,
            bottom: PageMargin::Auto,
            ..PAGE.margin
          },
          ..PAGE
        })
        .header(header)
        .footer(footer)
        .pages(PageRules {
          first: PageOverride {
            header: Some(cover.into()),
            ..PageOverride::default()
          },
          ..PageRules::default()
        })
        .producer("takumi-pdf test".to_string())
        .fonts(&fonts)
        .build(),
    )
    .expect("render with boxes");
    let pages = page_count(&rendered);
    let in_band = |origin: BoxOrigin, tag: &str| -> Vec<&PageBox> {
      boxes_of(&rendered, tag)
        .into_iter()
        .filter(|page_box| page_box.origin == origin)
        .collect()
    };
    // The bands set the margins they draw in.
    let content_top = |page: usize| {
      rendered
        .boxes
        .iter()
        .filter(|page_box| page_box.page == page && page_box.origin == BoxOrigin::Content)
        .map(|page_box| page_box.y)
        .fold(f32::INFINITY, f32::min)
    };
    let content_bottom = |page: usize| {
      rendered
        .boxes
        .iter()
        .filter(|page_box| page_box.page == page && page_box.origin == BoxOrigin::Content)
        .map(|page_box| page_box.y + page_box.height)
        .fold(0.0, f32::max)
    };

    assert!(pages > 1);
    let covers = in_band(BoxOrigin::Header, "strong");
    assert_eq!(covers.len(), 1);
    assert_eq!((covers[0].page, covers[0].rule), (0, Some(PageRule::First)));
    assert_eq!(Some(covers[0].source_order), strong);

    let titles = in_band(BoxOrigin::Header, "b");
    assert_eq!(titles.len(), pages - 1, "{titles:?}");
    for (page, title) in (1..).zip(&titles) {
      assert_eq!((title.page, title.rule), (page, None));
      // The page number filled in before it moves nothing.
      assert_eq!(Some(title.source_order), b);
      assert!(
        title.y + title.height <= content_top(page),
        "{title:?} leaves the header"
      );
    }

    let thanks = in_band(BoxOrigin::Footer, "i");
    assert_eq!(thanks.len(), pages);
    assert!(
      thanks
        .iter()
        .enumerate()
        .all(|(page, page_box)| page_box.page == page
          && Some(page_box.source_order) == i
          && page_box.rule.is_none()
          && page_box.y >= content_bottom(page))
    );
  }

  #[test]
  fn a_repeated_fixed_box_is_boxed_on_every_page() {
    let fonts = fonts();

    for stamp in [
      r#"<aside style="position: fixed; top: 0; right: 0; width: 40px; height: 20px;"><em>Draft</em></aside>"#,
      r#"<aside style="position: fixed; top: 0; right: 0; width: 40px; height: 20px;"><span class="pageNumber"></span> <em>Draft</em></aside>"#,
    ] {
      let node = html(&format!(
        r#"<div style="display: flex; flex-direction: column; font-size: 14px;">{stamp}<table style="width: 100%;"><tbody>{}</tbody></table></div>"#,
        rows()
      ));
      let (aside, em) = (
        preorder_index(&node, "aside", &mut 0),
        preorder_index(&node, "em", &mut 0),
      );
      let rendered = render_paged(&fonts, node);
      let pages = page_count(&rendered);

      assert!(pages > 1);
      for (tag, order) in [("aside", aside), ("em", em)] {
        let stamps = boxes_of(&rendered, tag);

        assert_eq!(stamps.len(), pages, "{tag} in {stamp}");
        assert!(
          stamps
            .iter()
            .enumerate()
            .all(|(page, page_box)| page_box.page == page
              && page_box.origin == BoxOrigin::Content
              && Some(page_box.source_order) == order),
          "{stamps:?}"
        );
      }
    }
  }

  #[test]
  fn an_inline_element_has_a_box_per_line_inside_its_parent() {
    let fonts = fonts();
    let node = html(
      r#"<p style="width: 120px; margin: 0; font-size: 14px;">Plain <b>bold text that wraps onto <i>more</i> lines</b> end</p>"#,
    );
    let (p, b, i) = (
      preorder_index(&node, "p", &mut 0),
      preorder_index(&node, "b", &mut 0),
      preorder_index(&node, "i", &mut 0),
    );
    let rendered = render_paged(&fonts, node);
    let paragraph = boxes_of(&rendered, "p");
    let bold = boxes_of(&rendered, "b");
    let italic = boxes_of(&rendered, "i");

    assert_eq!(paragraph.len(), 1);
    assert_eq!(paragraph[0].source_order, p.expect("a p"));
    assert!(bold.len() > 1, "the bold text wraps: {bold:?}");
    assert!(bold.iter().all(|page_box| Some(page_box.source_order) == b));
    assert!(bold.windows(2).all(|pair| pair[0].y < pair[1].y));
    assert_eq!(italic.len(), 1);
    assert_eq!(Some(italic[0].source_order), i);

    let contains = |outer: &PageBox, inner: &PageBox| {
      outer.x <= inner.x + 0.01
        && outer.y <= inner.y + 0.01
        && outer.x + outer.width >= inner.x + inner.width - 0.01
        && outer.y + outer.height >= inner.y + inner.height - 0.01
    };
    assert!(bold.iter().all(|line| contains(paragraph[0], line)));
    assert!(bold.iter().any(|line| contains(line, italic[0])));
    // The inner element paints over the outer one.
    let position = |page_box: &PageBox| rendered.boxes.iter().position(|other| other == page_box);
    assert!(position(italic[0]) > bold.iter().map(|line| position(line)).max().flatten());
  }
  #[test]
  fn an_inline_element_sits_inside_its_formatting_context_s_padding() {
    let fonts = fonts();
    let rendered = render_paged(
      &fonts,
      html(
        r#"<p style="margin: 0; padding: 20px; border: 5px solid red; font-size: 14px;">Plain <b>bold</b></p>"#,
      ),
    );
    let (paragraph, bold) = (boxes_of(&rendered, "p")[0], boxes_of(&rendered, "b")[0]);
    let inset = 25.0 * PT_PER_PX;

    assert!(
      bold.x >= paragraph.x + inset - 0.01,
      "{bold:?} in {paragraph:?}"
    );
    // A glyph's ascent can reach a little above its line.
    assert!(
      bold.y >= paragraph.y + inset - 1.0,
      "{bold:?} in {paragraph:?}"
    );
  }

  #[test]
  fn inline_elements_around_a_block_are_boxed() {
    let fonts = fonts();
    let rendered = render_paged(
      &fonts,
      html(r#"<div style="font-size: 14px;"><b>before</b><p>block</p><i>after</i></div>"#),
    );

    assert_eq!(boxes_of(&rendered, "b").len(), 1);
    assert_eq!(boxes_of(&rendered, "i").len(), 1);
  }

  #[test]
  fn hidden_text_has_no_box() {
    let fonts = fonts();
    let rendered = render_paged(
      &fonts,
      html(r#"<p>Plain <b style="visibility: hidden">hidden</b><i>shown</i></p>"#),
    );

    assert!(boxes_of(&rendered, "b").is_empty());
    assert_eq!(boxes_of(&rendered, "i").len(), 1);
  }

  #[test]
  fn inline_elements_come_after_what_paints_under_their_text() {
    let fonts = fonts();
    let rendered = render_paged(
      &fonts,
      html(
        r#"<p style="position: relative; z-index: 0; font-size: 14px;"><b>front</b><span style="display: inline-block; position: relative; left: -40px; z-index: -1; background: red;">behind</span></p>"#,
      ),
    );
    let position = |tag: &str| {
      rendered
        .boxes
        .iter()
        .position(|page_box| page_box.tag_name.as_deref() == Some(tag))
    };

    assert!(position("p") < position("span"));
    assert!(position("span") < position("b"));
  }

  #[test]
  fn a_band_s_emptied_target_counter_keeps_its_source_order() {
    let fonts = fonts();
    let header =
      html(r##"<header><span class="targetPageNumber"><i>old</i></span><b>Title</b></header>"##);
    let title = preorder_index(&header, "b", &mut 0);
    let rendered = render_with_boxes(
      PdfOptions::builder()
        .node(document())
        .page(PAGE)
        .header(header)
        .producer("takumi-pdf test".to_string())
        .fonts(&fonts)
        .build(),
    )
    .expect("render with boxes");
    let titles = boxes_of(&rendered, "b");

    assert!(!titles.is_empty());
    assert!(
      titles
        .iter()
        .all(|page_box| Some(page_box.source_order) == title)
    );
  }
}
