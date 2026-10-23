// SPDX-License-Identifier: AGPL-3.0-or-later
//! A small flexbox: rows and columns of fixed, fill and fit children, plus
//! absolutely positioned children (timeline clips, overlays).

use crate::TextMeasure;
use crate::geometry::{Rect, Size};
use crate::node::{Align, Direction, Justify, Node, Sizing};

/// Lays out `root` to fill `rect`.
pub(crate) fn layout(root: &mut Node, rect: Rect, text: &mut dyn TextMeasure) {
    measure(root, text);
    arrange(root, rect);
}

/// Computes every node's content-fit size, bottom-up. Absolutely positioned
/// children don't count towards their parent's size.
fn measure(node: &mut Node, text: &mut dyn TextMeasure) -> Size {
    let mut content = Size::default();
    if let Some(t) = &node.text {
        content = text.measure(t, &node.style.text_style());
    }
    let flowing = node
        .children
        .iter()
        .filter(|c| c.position.is_none())
        .count();
    let gaps = node.gap * flowing.saturating_sub(1) as f32;
    let (mut main, mut cross) = (gaps, 0.0f32);
    for child in &mut node.children {
        let fit = measure(child, text);
        if child.position.is_some() {
            continue;
        }
        let w = fixed_or(child.width, fit.w);
        let h = fixed_or(child.height, fit.h);
        let (m, c) = along(node.direction, w, h);
        main += m;
        cross = cross.max(c);
    }
    if flowing > 0 {
        let (w, h) = unalong(node.direction, main, cross);
        content = Size::new(content.w.max(w), content.h.max(h));
    }
    node.fit = Size::new(
        content.w + node.padding.horizontal(),
        content.h + node.padding.vertical(),
    );
    node.fit
}

fn arrange(node: &mut Node, rect: Rect) {
    node.rect = rect;
    let inner = rect.inset(node.padding);
    for child in &mut node.children {
        if let Some(p) = child.position {
            arrange(child, Rect::new(inner.x + p.x, inner.y + p.y, p.w, p.h));
        }
    }
    let flowing = node
        .children
        .iter()
        .filter(|c| c.position.is_none())
        .count();
    if flowing == 0 {
        return;
    }
    let dir = node.direction;
    let (inner_main, inner_cross) = along(dir, inner.w, inner.h);
    let gaps = node.gap * (flowing - 1) as f32;

    // Main-axis sizes: fixed and fit first, then fill shares the rest.
    let main_sizing = |c: &Node| match dir {
        Direction::Row => c.width,
        Direction::Column => c.height,
    };
    let fit_main = |c: &Node| along(dir, c.fit.w, c.fit.h).0;
    let mut used = gaps;
    let mut weights = 0.0;
    for child in node.children.iter().filter(|c| c.position.is_none()) {
        match main_sizing(child) {
            Sizing::Fixed(v) => used += v,
            Sizing::Fit => used += fit_main(child),
            Sizing::Fill(w) => weights += w,
        }
    }
    let free = (inner_main - used).max(0.0);

    let mut pos = if weights > 0.0 {
        0.0
    } else {
        match node.justify {
            Justify::Start => 0.0,
            Justify::Center => free / 2.0,
            Justify::End => free,
        }
    };
    let (align, gap) = (node.align, node.gap);
    for child in node.children.iter_mut().filter(|c| c.position.is_none()) {
        let main = match main_sizing(child) {
            Sizing::Fixed(v) => v,
            Sizing::Fit => fit_main(child),
            Sizing::Fill(w) => free * w / weights,
        };
        let cross_sizing = match dir {
            Direction::Row => child.height,
            Direction::Column => child.width,
        };
        let cross = match cross_sizing {
            Sizing::Fixed(v) => v,
            Sizing::Fit => along(dir, child.fit.w, child.fit.h).1,
            Sizing::Fill(_) => inner_cross,
        };
        let cross_pos = match align {
            Align::Start => 0.0,
            Align::Center => (inner_cross - cross) / 2.0,
            Align::End => inner_cross - cross,
        };
        let (x, y) = unalong(dir, pos, cross_pos);
        let (w, h) = unalong(dir, main, cross);
        arrange(child, Rect::new(inner.x + x, inner.y + y, w, h));
        pos += main + gap;
    }
}

fn fixed_or(sizing: Sizing, fit: f32) -> f32 {
    match sizing {
        Sizing::Fixed(v) => v,
        _ => fit,
    }
}

/// (width, height) → (main, cross) for `dir`.
fn along(dir: Direction, w: f32, h: f32) -> (f32, f32) {
    match dir {
        Direction::Row => (w, h),
        Direction::Column => (h, w),
    }
}

/// (main, cross) → (x or width, y or height) for `dir`.
fn unalong(dir: Direction, main: f32, cross: f32) -> (f32, f32) {
    along(dir, main, cross)
}
