//! Where each element lands on the pages, for tools that map a page back to its source.

use std::iter::{repeat_n, successors};

use takumi_core::{
  geometry::{Point as CorePoint, Size},
  layout::node::{Node, NodeKind},
  paint_property::{ClipKind, ClipNode},
  scene::{NodePaint, SceneView},
};

use crate::{
  emitter::shape_extents,
  interactive::{PaintedBox, page_rect, transformed_rect},
  krilla::geom::Rect as KrillaRect,
  tree::INPUT_ROOT,
};

/// One fragment of an element on one page, in pt from the page's top-left corner.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct PageBox {
  /// The output page, from 0.
  pub page: usize,
  /// Its position in the input tree, every node counted in document order from 0 at the root.
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

/// The boxes a render collects as its pages draw.
#[derive(Default)]
pub(crate) struct BoxCollector {
  pub(crate) boxes: Vec<PageBox>,
  /// Present when filling in counters changed the tree that was laid out.
  pub(crate) orders: Option<SourceOrders>,
}

impl BoxCollector {
  /// Adds the boxes `scene` leaves visible in `window`, the page's area in content coordinates.
  pub(crate) fn collect(
    &mut self,
    scene: SceneView<'_>,
    page: usize,
    window: KrillaRect,
    offset: CorePoint<f32>,
  ) {
    scene.for_each_paint(|paint| {
      if let Some(page_box) = self.page_box(scene, paint, page, window, offset) {
        self.boxes.push(page_box);
      }
    });
  }

  fn page_box(
    &self,
    scene: SceneView<'_>,
    paint: &NodePaint,
    page: usize,
    window: KrillaRect,
    offset: CorePoint<f32>,
  ) -> Option<PageBox> {
    let painted = PaintedBox::of(scene, paint)?;
    let laid_out = painted.node.source_order()?.checked_sub(INPUT_ROOT)?;
    let source_order = match &self.orders {
      Some(orders) => orders.input_order(laid_out)?,
      None => laid_out,
    };
    let visible = visible_rect(scene, paint, painted.rect, window)?;
    let rect = page_rect(visible, (window.top(), window.bottom()), offset)?;

    Some(PageBox {
      page,
      source_order,
      tag_name: painted.source.tag_name().map(str::to_owned),
      x: rect.left(),
      y: rect.top(),
      width: rect.width(),
      height: rect.height(),
    })
  }
}

/// The part of `rect` the clips it paints under and the page `window` leave visible.
fn visible_rect(
  scene: SceneView<'_>,
  paint: &NodePaint,
  rect: KrillaRect,
  window: KrillaRect,
) -> Option<KrillaRect> {
  successors(paint.properties.border_box.clip, |id| {
    scene.properties.clip(*id).parent
  })
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
    PageBox, PageMargins, PageOptions, PdfOptions, RenderedPdf, options::PT_PER_PX, render,
    render_with_boxes, test_support::fonts,
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
}
