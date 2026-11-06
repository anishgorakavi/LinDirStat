use std::cell::RefCell;

use iced::mouse;
use iced::widget::canvas::{self, Action, Path, Stroke, Text};
use iced::{Color, Event, Pixels, Point, Rectangle, Renderer, Size, Theme};

use crate::scan::Tree;
use crate::Message;

#[derive(Clone, Copy, Debug)]
pub struct Cell {
    pub id: usize,
    pub rect: Rectangle,
    pub leaf: bool,
}

fn worst(row: &[f64], sum: f64, short: f64) -> f64 {
    let max = row.iter().cloned().fold(f64::MIN, f64::max);
    let min = row.iter().cloned().fold(f64::MAX, f64::min);
    ((short * short * max) / (sum * sum)).max((sum * sum) / (short * short * min))
}

/// Squarified treemap (Bruls et al.). `sizes` must be positive and descending.
fn squarify(sizes: &[f64], rect: Rectangle) -> Vec<Rectangle> {
    let total: f64 = sizes.iter().sum();
    let scale = (rect.width as f64 * rect.height as f64) / total;
    let areas: Vec<f64> = sizes.iter().map(|s| s * scale).collect();
    let (mut x, mut y) = (rect.x as f64, rect.y as f64);
    let (mut w, mut h) = (rect.width as f64, rect.height as f64);
    let mut out = Vec::with_capacity(areas.len());
    let mut i = 0;
    while i < areas.len() {
        let short = w.min(h).max(1e-6);
        let mut j = i + 1;
        let mut sum = areas[i];
        let mut cur = worst(&areas[i..j], sum, short);
        while j < areas.len() {
            let ns = sum + areas[j];
            let nw = worst(&areas[i..=j], ns, short);
            if nw <= cur {
                sum = ns;
                cur = nw;
                j += 1;
            } else {
                break;
            }
        }
        let thick = sum / short;
        if w >= h {
            let mut yy = y;
            for a in &areas[i..j] {
                let hh = a / thick;
                out.push(Rectangle::new(
                    Point::new(x as f32, yy as f32),
                    Size::new(thick as f32, hh as f32),
                ));
                yy += hh;
            }
            x += thick;
            w -= thick;
        } else {
            let mut xx = x;
            for a in &areas[i..j] {
                let ww = a / thick;
                out.push(Rectangle::new(
                    Point::new(xx as f32, y as f32),
                    Size::new(ww as f32, thick as f32),
                ));
                xx += ww;
            }
            y += thick;
            h -= thick;
        }
        i = j;
    }
    out
}

fn shrink(r: Rectangle, by: f32) -> Rectangle {
    Rectangle::new(
        Point::new(r.x + by, r.y + by),
        Size::new(
            (r.width - 2.0 * by).max(0.0),
            (r.height - 2.0 * by).max(0.0),
        ),
    )
}

/// Folders that were not read have no known size; give them a small fixed
/// share of their parent so they stay visible and clickable.
fn weight(tree: &Tree, c: usize, unscanned_w: f64) -> f64 {
    let n = &tree.nodes[c];
    if n.is_dir && !n.scanned {
        unscanned_w
    } else {
        n.size as f64
    }
}

fn place(tree: &Tree, id: usize, rect: Rectangle, out: &mut Vec<Cell>) {
    let scanned_total: f64 = tree.nodes[id]
        .children
        .iter()
        .map(|&c| &tree.nodes[c])
        .filter(|n| !(n.is_dir && !n.scanned))
        .map(|n| n.size as f64)
        .sum();
    let unscanned_w = (scanned_total * 0.03).max(1.0);
    let mut kids: Vec<(usize, f64)> = tree.nodes[id]
        .children
        .iter()
        .map(|&c| (c, weight(tree, c, unscanned_w)))
        .filter(|&(_, w)| w > 0.0)
        .collect();
    kids.sort_by(|a, b| b.1.total_cmp(&a.1));
    if kids.is_empty() || rect.width < 1.0 || rect.height < 1.0 {
        out.push(Cell {
            id,
            rect,
            leaf: true,
        });
        return;
    }
    let sizes: Vec<f64> = kids.iter().map(|k| k.1).collect();
    for (&(c, _), r) in kids.iter().zip(squarify(&sizes, rect)) {
        if r.width < 1.0 || r.height < 1.0 {
            continue;
        }
        let n = &tree.nodes[c];
        if n.is_dir && !n.children.is_empty() && r.width >= 8.0 && r.height >= 8.0 {
            out.push(Cell {
                id: c,
                rect: r,
                leaf: false,
            });
            place(tree, c, shrink(r, 1.0), out);
        } else {
            out.push(Cell {
                id: c,
                rect: r,
                leaf: true,
            });
        }
    }
}

pub fn build(tree: &Tree, root: usize, size: Size) -> Vec<Cell> {
    let mut out = Vec::new();
    place(tree, root, Rectangle::new(Point::ORIGIN, size), &mut out);
    out
}

pub struct Treemap<'a> {
    pub tree: &'a Tree,
    pub root: usize,
    pub rev: u64,
    pub selected: Option<usize>,
}

#[derive(Default)]
pub struct State {
    cache: canvas::Cache,
    layout: RefCell<Option<(Size, usize, u64, Vec<Cell>)>>,
    hover: Option<usize>,
}

impl Treemap<'_> {
    fn with_layout<R>(&self, state: &State, size: Size, f: impl FnOnce(&[Cell]) -> R) -> R {
        let mut l = state.layout.borrow_mut();
        let stale = match &*l {
            Some((s, r, v, _)) => *s != size || *r != self.root || *v != self.rev,
            None => true,
        };
        if stale {
            *l = Some((size, self.root, self.rev, build(self.tree, self.root, size)));
            state.cache.clear();
        }
        f(&l.as_ref().unwrap().3)
    }

    fn hit(&self, state: &State, size: Size, p: Point) -> Option<usize> {
        self.with_layout(state, size, |cells| {
            cells
                .iter()
                .rev()
                .find(|c| c.leaf && c.rect.contains(p))
                .map(|c| c.id)
        })
    }
}

impl canvas::Program<Message> for Treemap<'_> {
    type State = State;

    fn update(
        &self,
        state: &mut State,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<Action<Message>> {
        match event {
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let hit = cursor
                    .position_in(bounds)
                    .and_then(|p| self.hit(state, bounds.size(), p));
                if hit != state.hover {
                    state.hover = hit;
                    return Some(Action::request_redraw());
                }
            }
            Event::Mouse(mouse::Event::CursorLeft) if state.hover.is_some() => {
                state.hover = None;
                return Some(Action::request_redraw());
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let Some(id) = cursor
                    .position_in(bounds)
                    .and_then(|p| self.hit(state, bounds.size(), p))
                {
                    return Some(Action::publish(Message::Select(id)).and_capture());
                }
            }
            _ => {}
        }
        None
    }

    fn draw(
        &self,
        state: &State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let size = bounds.size();
        self.with_layout(state, size, |_| ());

        let base = state.cache.draw(renderer, size, |frame| {
            let l = state.layout.borrow();
            let Some((_, _, _, cells)) = l.as_ref() else {
                return;
            };
            // One fill per block; a half-pixel inset separates neighbours.
            for c in cells.iter().filter(|c| c.leaf) {
                let r = shrink(c.rect, 0.5);
                frame.fill_rectangle(r.position(), r.size(), self.tree.color_of(c.id));
            }
            for c in cells
                .iter()
                .filter(|c| c.leaf && c.rect.width > 64.0 && c.rect.height > 16.0)
            {
                let n = &self.tree.nodes[c.id];
                let full = if self.tree.live && n.is_dir && !n.scanned {
                    format!("▸ {} (scanning)", n.name)
                } else if n.is_dir && !n.scanned {
                    format!("▸ {} (not scanned)", n.name)
                } else {
                    n.name.clone()
                };
                let max_chars = ((c.rect.width - 6.0) / 6.5) as usize;
                let mut name: String = full.chars().take(max_chars).collect();
                if name.chars().count() < full.chars().count() && max_chars > 1 {
                    name.pop();
                    name.push('…');
                }
                frame.fill_text(Text {
                    content: name,
                    position: Point::new(c.rect.x + 3.0, c.rect.y + 2.0),
                    color: Color::from_rgba(1.0, 1.0, 1.0, 0.9),
                    size: Pixels(11.0),
                    ..Text::default()
                });
            }
            let edge = Stroke::default().with_color(Color::BLACK).with_width(1.0);
            for c in cells.iter().filter(|c| !c.leaf) {
                frame.stroke(&Path::rectangle(c.rect.position(), c.rect.size()), edge);
            }
        });

        let mut overlay = canvas::Frame::new(renderer, size);
        let l = state.layout.borrow();
        if let Some((_, _, _, cells)) = l.as_ref() {
            let mut outline = |id: usize, color: Color, width: f32| {
                if let Some(c) = cells.iter().find(|c| c.id == id) {
                    overlay.stroke(
                        &Path::rectangle(c.rect.position(), c.rect.size()),
                        Stroke::default().with_color(color).with_width(width),
                    );
                }
            };
            if let Some(h) = state.hover {
                outline(h, Color::WHITE, 1.5);
            }
            if let Some(s) = self.selected {
                outline(s, Color::from_rgb(1.0, 0.9, 0.1), 3.0);
            }
        }
        drop(l);
        if let Some(h) = state.hover {
            let n = &self.tree.nodes[h];
            let text = format!(
                "{} — {} — modified {}",
                self.tree.path(h).display(),
                self.tree.size_label(h),
                crate::util::ago(n.mtime)
            );
            let w = (text.chars().count() as f32 * 6.6 + 12.0).min(size.width);
            overlay.fill_rectangle(
                Point::new(0.0, size.height - 22.0),
                Size::new(w, 22.0),
                Color::from_rgba(0.0, 0.0, 0.0, 0.8),
            );
            overlay.fill_text(Text {
                content: text,
                position: Point::new(6.0, size.height - 17.0),
                color: Color::WHITE,
                size: Pixels(12.0),
                ..Text::default()
            });
        }
        vec![base, overlay.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &State,
        _b: Rectangle,
        _c: mouse::Cursor,
    ) -> mouse::Interaction {
        if state.hover.is_some() {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn squarify_fills_rect_exactly() {
        let sizes = [60.0, 30.0, 20.0, 10.0, 5.0, 5.0];
        let r = Rectangle::new(Point::new(0.0, 0.0), Size::new(400.0, 300.0));
        let rects = squarify(&sizes, r);
        assert_eq!(rects.len(), sizes.len());
        let area: f32 = rects.iter().map(|r| r.width * r.height).sum();
        assert!((area - 120_000.0).abs() < 5.0, "area {area}");
        for (i, s) in sizes.iter().enumerate() {
            let expect = 120_000.0 * (*s as f32) / 130.0;
            assert!((rects[i].width * rects[i].height - expect).abs() < 5.0);
        }
    }
}
