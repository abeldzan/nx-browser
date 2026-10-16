//! Tiled windows as a split tree, the way tiling window managers keep them: every split divides its
//! rectangle between two children at a ratio, so the leaves always fill the stage. Frames are laid out
//! from the tree, never the other way round; geometry is only read to rebuild a tree after a restart.
//!
//! Everything here works in cells: frames grown by half the inset gap, so neighbouring cells touch.

use super::{Frame, MIN_H, MIN_W, SNAP_INSET};

pub(super) fn cell(f: Frame) -> Frame {
    Frame { x: f.x - SNAP_INSET / 2.0, y: f.y - SNAP_INSET / 2.0, w: f.w + SNAP_INSET, h: f.h + SNAP_INSET }
}

pub(super) fn uncell(c: Frame) -> Frame {
    Frame { x: c.x + SNAP_INSET / 2.0, y: c.y + SNAP_INSET / 2.0, w: (c.w - SNAP_INSET).max(1.0), h: (c.h - SNAP_INSET).max(1.0) }
}

/// Start and end of `f` along x (`across`), or along y.
pub(super) fn span(f: Frame, across: bool) -> (f32, f32) {
    if across { (f.x, f.x + f.w) } else { (f.y, f.y + f.h) }
}

pub(super) fn set_span(f: &mut Frame, across: bool, (lo, hi): (f32, f32)) {
    if across {
        (f.x, f.w) = (lo, hi - lo);
    } else {
        (f.y, f.h) = (lo, hi - lo);
    }
}

/// Which side of a window another one is put on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

impl Side {
    /// The side of `rect` nearest to (x, y).
    pub(super) fn nearest(rect: Frame, x: f32, y: f32) -> Side {
        let u = ((x - rect.x) / rect.w.max(1.0)).clamp(0.0, 1.0);
        let v = ((y - rect.y) / rect.h.max(1.0)).clamp(0.0, 1.0);
        [(u, Side::Left), (1.0 - u, Side::Right), (v, Side::Top), (1.0 - v, Side::Bottom)]
            .into_iter()
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map_or(Side::Right, |(_, side)| side)
    }

    fn across(self) -> bool {
        matches!(self, Side::Left | Side::Right)
    }

    fn first(self) -> bool {
        matches!(self, Side::Left | Side::Top)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Tile {
    Leaf(String),
    /// `across`: the children sit side by side; else one above the other. `ratio` is the first child's share.
    Split { across: bool, ratio: f32, first: Box<Tile>, second: Box<Tile> },
}

/// Splits `rect` along `across` at `ratio`.
fn divide(rect: Frame, across: bool, ratio: f32) -> (Frame, Frame) {
    let (lo, hi) = span(rect, across);
    let at = lo + (hi - lo) * ratio;
    let (mut a, mut b) = (rect, rect);
    set_span(&mut a, across, (lo, at));
    set_span(&mut b, across, (at, hi));
    (a, b)
}

impl Tile {
    pub(super) fn contains(&self, id: &str) -> bool {
        match self {
            Tile::Leaf(leaf) => leaf == id,
            Tile::Split { first, second, .. } => first.contains(id) || second.contains(id),
        }
    }

    pub(super) fn leaves(&self) -> Vec<String> {
        self.layout(Frame { x: 0.0, y: 0.0, w: 1.0, h: 1.0 }).into_iter().map(|(id, _)| id).collect()
    }

    /// Each leaf's cell when the tree fills `rect`.
    pub(super) fn layout(&self, rect: Frame) -> Vec<(String, Frame)> {
        let mut out = Vec::new();
        self.lay(rect, &mut out);
        out
    }

    fn lay(&self, rect: Frame, out: &mut Vec<(String, Frame)>) {
        match self {
            Tile::Leaf(id) => out.push((id.clone(), rect)),
            Tile::Split { across, ratio, first, second } => {
                let (a, b) = divide(rect, *across, *ratio);
                first.lay(a, out);
                second.lay(b, out);
            }
        }
    }

    /// The tree without leaf `id`: its sibling takes the whole of their split. None when nothing is left.
    pub(super) fn without(self, id: &str) -> Option<Tile> {
        match self {
            Tile::Leaf(leaf) if leaf == id => None,
            Tile::Leaf(_) => Some(self),
            Tile::Split { across, ratio, first, second } => match (first.without(id), second.without(id)) {
                (Some(first), Some(second)) => Some(Tile::Split { across, ratio, first: Box::new(first), second: Box::new(second) }),
                (one, other) => one.or(other),
            },
        }
    }

    /// Splits leaf `target` in half, putting `id` on `side` of it. Without `target`, splits the whole tree.
    pub(super) fn insert(self, target: &str, id: &str, side: Side) -> Tile {
        if !self.contains(target) {
            return self.beside(id, side);
        }
        match self {
            Tile::Leaf(leaf) if leaf == target => Tile::Leaf(leaf).beside(id, side),
            Tile::Split { across, ratio, first, second } => Tile::Split {
                across,
                ratio,
                first: Box::new(if first.contains(target) { first.insert(target, id, side) } else { *first }),
                second: Box::new(if second.contains(target) { second.insert(target, id, side) } else { *second }),
            },
            leaf => leaf,
        }
    }

    /// This tree and leaf `id` side by side, halves each.
    pub(super) fn beside(self, id: &str, side: Side) -> Tile {
        let new = Box::new(Tile::Leaf(id.to_string()));
        let (first, second) = if side.first() { (new, Box::new(self)) } else { (Box::new(self), new) };
        Tile::Split { across: side.across(), ratio: 0.5, first, second }
    }

    /// Trades the places of leaves `a` and `b`; a leaf only one of them has passes to the other.
    pub(super) fn swap(&mut self, a: &str, b: &str) {
        match self {
            Tile::Leaf(leaf) if leaf == a => *leaf = b.to_string(),
            Tile::Leaf(leaf) if leaf == b => *leaf = a.to_string(),
            Tile::Leaf(_) => {}
            Tile::Split { first, second, .. } => {
                first.swap(a, b);
                second.swap(a, b);
            }
        }
    }

    /// The least room this tree needs along `across` for every leaf to keep the minimum window size.
    fn min_len(&self, across: bool) -> f32 {
        match self {
            Tile::Leaf(_) => (if across { MIN_W } else { MIN_H }) + SNAP_INSET,
            Tile::Split { across: own, first, second, .. } if *own == across => first.min_len(across) + second.min_len(across),
            Tile::Split { first, second, .. } => first.min_len(across).max(second.min_len(across)),
        }
    }

    /// Moves the border of leaf `id` on its far side (`after`) or near side along `across` to `pos`.
    /// The nearest split owning that border moves; everything inside its two halves scales along.
    /// False when no split owns it: the border is the stage edge.
    pub(super) fn set_divider(&mut self, rect: Frame, id: &str, across: bool, after: bool, pos: f32) -> bool {
        let Tile::Split { across: own, ratio, first, second } = self else {
            return false;
        };
        let (a, b) = divide(rect, *own, *ratio);
        let in_first = first.contains(id);
        let deeper = if in_first { first.set_divider(a, id, across, after, pos) } else { second.set_divider(b, id, across, after, pos) };
        if deeper {
            return true;
        }
        if *own != across || in_first != after {
            return false;
        }
        let (lo, hi) = span(rect, across);
        let len = (hi - lo).max(1.0);
        let (low, high) = (first.min_len(across) / len, 1.0 - second.min_len(across) / len);
        if low <= high {
            *ratio = ((pos - lo) / len).clamp(low, high);
        }
        true
    }

    /// Rebuilds a tree from cells laid out by one, as saved before a restart. Cells that overlap (floating
    /// windows being tiled) split at the widest gap between their centres, so each keeps about its place.
    pub(super) fn infer(cells: &[(String, Frame)]) -> Option<Tile> {
        let frames = cells.iter().map(|(_, frame)| *frame).collect::<Vec<_>>();
        let items = (0..cells.len()).collect::<Vec<_>>();
        (!items.is_empty()).then(|| build(&items, &frames, cells))
    }
}

fn build(items: &[usize], frames: &[Frame], cells: &[(String, Frame)]) -> Tile {
    if let [one] = items {
        return Tile::Leaf(cells[*one].0.clone());
    }
    let (across, groups, clean) = split(items, frames);
    // Only non-finite frames fail to split; tiling them would index past `bounds` or recurse on the same items.
    if groups.len() < 2 {
        return Tile::Leaf(cells[items[0]].0.clone());
    }
    let bounds = groups.iter().map(|group| extent(group, frames, across)).collect::<Vec<_>>();
    // Borders sit halfway across the gap between neighbouring groups.
    let mut borders = vec![bounds[0].0];
    borders.extend(bounds.windows(2).map(|pair| (pair[0].1 + pair[1].0) / 2.0));
    borders.push(bounds[bounds.len() - 1].1);
    // Folded from the end, so each split holds one group and the ones after it.
    let mut tree = build(&groups[groups.len() - 1], frames, cells);
    for n in (0..groups.len() - 1).rev() {
        let (lo, at, hi) = (borders[n], borders[n + 1], borders[borders.len() - 1]);
        let ratio = ((at - lo) / (hi - lo).max(1.0)).clamp(if clean { 0.05 } else { 0.25 }, if clean { 0.95 } else { 0.75 });
        tree = Tile::Split { across, ratio, first: Box::new(build(&groups[n], frames, cells)), second: Box::new(tree) };
    }
    tree
}

fn extent(items: &[usize], frames: &[Frame], across: bool) -> (f32, f32) {
    items.iter().map(|&i| span(frames[i], across)).fold((f32::MAX, f32::MIN), |(lo, hi), (a, b)| (lo.min(a), hi.max(b)))
}

/// How far apart two cell edges may be and still count as one border.
const CUT_SLOP: f32 = 2.0;

/// `items` grouped, in order, into the slabs that clean cuts across `across` (lines no cell straddles) divide them into.
pub(super) fn slabs(items: &[usize], frames: &[Frame], across: bool) -> Vec<Vec<usize>> {
    let mut sorted = items.to_vec();
    sorted.sort_by(|a, b| span(frames[*a], across).0.total_cmp(&span(frames[*b], across).0));
    let mut out: Vec<Vec<usize>> = Vec::new();
    let mut end = f32::MIN;
    for i in sorted {
        let (lo, hi) = span(frames[i], across);
        match out.last_mut() {
            Some(slab) if lo < end - CUT_SLOP => slab.push(i),
            _ => out.push(vec![i]),
        }
        end = end.max(hi);
    }
    out
}

/// How `items` divide: along clean cuts if there are any (columns first), else in two at the widest gap
/// between their centres. Also says along which axis, and whether the cut was clean.
fn split(items: &[usize], frames: &[Frame]) -> (bool, Vec<Vec<usize>>, bool) {
    for across in [true, false] {
        let groups = slabs(items, frames, across);
        if groups.len() > 1 {
            return (across, groups, true);
        }
    }
    let centre = |i: usize, across: bool| {
        let (a, b) = span(frames[i], across);
        (a + b) / 2.0
    };
    let mut best = (f32::MIN, true, Vec::new());
    for across in [true, false] {
        let mut sorted = items.to_vec();
        sorted.sort_by(|a, b| centre(*a, across).total_cmp(&centre(*b, across)));
        for n in 1..sorted.len() {
            let gap = centre(sorted[n], across) - centre(sorted[n - 1], across);
            if gap > best.0 {
                best = (gap, across, vec![sorted[..n].to_vec(), sorted[n..].to_vec()]);
            }
        }
    }
    (best.1, best.2, false)
}

/// Stretches `items` along `across` from `from` to `to`, each keeping its share.
fn remap(items: &[usize], frames: &mut [Frame], across: bool, from: (f32, f32), to: (f32, f32)) {
    let k = (to.1 - to.0) / (from.1 - from.0).max(1.0);
    for &i in items {
        let (lo, hi) = span(frames[i], across);
        set_span(&mut frames[i], across, (to.0 + (lo - from.0) * k, to.0 + (hi - from.0) * k));
    }
}

/// For windows snapped to the stage's halves and quarters, which have no tree: moves the border of cell `who`
/// at `line` along `across` to `to`, with the neighbouring slabs across it. False when the edge borders no cell.
pub(super) fn move_border(frames: &mut [Frame], who: usize, across: bool, line: f32, to: f32) -> bool {
    let min = if across { MIN_W } else { MIN_H } + SNAP_INSET;
    let mut items = (0..frames.len()).collect::<Vec<_>>();
    loop {
        let along = slabs(&items, frames, across);
        let bounds = along.iter().map(|slab| extent(slab, frames, across)).collect::<Vec<_>>();
        if let Some(k) = (1..along.len()).find(|&k| (bounds[k].0 - line).abs() <= CUT_SLOP) {
            let (lo, hi) = (bounds[k - 1].0, bounds[k].1);
            // No cell may shrink below the minimum, nor below what it already is.
            let smallest = |slab: &[usize]| slab.iter().map(|&i| span(frames[i], across)).map(|(a, b)| b - a).fold(f32::MAX, f32::min);
            let floor = lo + (line - lo) * (min / smallest(&along[k - 1])).min(1.0);
            let ceil = hi - (hi - line) * (min / smallest(&along[k])).min(1.0);
            let to = if floor > ceil { line } else { to.clamp(floor, ceil) };
            remap(&along[k - 1], frames, across, (lo, line), (lo, to));
            remap(&along[k], frames, across, (line, hi), (to, hi));
            return true;
        }
        let groups = if along.len() > 1 { along } else { slabs(&items, frames, !across) };
        if groups.len() < 2 {
            return false;
        }
        let Some(inner) = groups.into_iter().find(|slab| slab.contains(&who)) else {
            return false;
        };
        items = inner;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(x: f32, y: f32, w: f32, h: f32) -> Frame {
        Frame { x, y, w, h }
    }

    fn leaf(id: &str) -> Tile {
        Tile::Leaf(id.into())
    }

    fn boxes(tree: &Tile, rect: Frame) -> Vec<(String, (f32, f32, f32, f32))> {
        tree.layout(rect).into_iter().map(|(id, c)| (id, (c.x, c.y, c.w, c.h))).collect()
    }

    const STAGE: Frame = Frame { x: 0.0, y: 0.0, w: 1000.0, h: 600.0 };

    #[test]
    fn new_windows_split_and_always_fill_the_stage() {
        let tree = leaf("a").beside("b", Side::Right).insert("b", "c", Side::Bottom);
        assert_eq!(
            boxes(&tree, STAGE),
            vec![("a".into(), (0., 0., 500., 600.)), ("b".into(), (500., 0., 500., 300.)), ("c".into(), (500., 300., 500., 300.))]
        );
        // Closing one hands its space to its sibling.
        let tree = tree.without("b").unwrap();
        assert_eq!(boxes(&tree, STAGE), vec![("a".into(), (0., 0., 500., 600.)), ("c".into(), (500., 0., 500., 600.))]);
        assert_eq!(leaf("a").without("a"), None);
    }

    #[test]
    fn dragging_a_border_moves_its_split_and_scales_whats_inside() {
        // a | (b above (c | d))
        let mut tree = leaf("a").beside("b", Side::Right).insert("b", "c", Side::Bottom).insert("c", "d", Side::Right);
        assert!(tree.set_divider(STAGE, "a", true, true, 400.));
        let laid = boxes(&tree, STAGE);
        assert_eq!(laid[0].1, (0., 0., 400., 600.));
        // c and d keep their 1:1 split of the wider right side.
        assert_eq!((laid[2].1 .0, laid[2].1 .2, laid[3].1 .0, laid[3].1 .2), (400., 300., 700., 300.));
        // The border between c and d belongs to their own split; a stays.
        assert!(tree.set_divider(STAGE, "d", true, false, 650.));
        let laid = boxes(&tree, STAGE);
        assert_eq!((laid[0].1 .2, laid[2].1 .2, laid[3].1 .0), (400., 250., 650.));
        // The stage edge belongs to no split.
        assert!(!tree.set_divider(STAGE, "a", true, false, 50.));
        // Borders stop where a window would get too small.
        assert!(tree.set_divider(STAGE, "a", true, true, 990.));
        assert_eq!(boxes(&tree, STAGE)[0].1 .2, 1000. - 2. * (MIN_W + SNAP_INSET));
    }

    #[test]
    fn swapping_trades_places() {
        let mut tree = leaf("a").beside("b", Side::Right);
        tree.swap("a", "b");
        assert_eq!(tree.leaves(), vec!["b".to_string(), "a".to_string()]);
        tree.swap("a", "z");
        assert_eq!(tree.leaves(), vec!["b".to_string(), "z".to_string()]);
    }

    #[test]
    fn side_follows_the_nearest_edge() {
        let rect = f(0., 0., 100., 100.);
        assert_eq!(Side::nearest(rect, 10., 50.), Side::Left);
        assert_eq!(Side::nearest(rect, 50., 95.), Side::Bottom);
    }

    #[test]
    fn a_laid_out_tree_rebuilds_from_its_cells() {
        let tree = leaf("a").beside("b", Side::Right).insert("b", "c", Side::Bottom);
        let mut tree = tree;
        tree.set_divider(STAGE, "a", true, true, 300.);
        let rebuilt = Tile::infer(&tree.layout(STAGE)).unwrap();
        assert_eq!(boxes(&rebuilt, STAGE), boxes(&tree, STAGE));
        // Overlapping floating windows: left one goes left, the other right, filling the stage between them.
        let rebuilt = Tile::infer(&[("r".into(), f(500., 100., 400., 300.)), ("l".into(), f(50., 50., 400., 300.))]).unwrap();
        let laid = boxes(&rebuilt, STAGE);
        assert_eq!((laid[0].0.as_str(), laid[0].1 .0, laid[1].1 .0 + laid[1].1 .2), ("l", 0., 1000.));
    }

    #[test]
    fn snapped_halves_share_a_border() {
        let mut frames = vec![f(0., 0., 500., 600.), f(500., 0., 500., 600.)];
        assert!(move_border(&mut frames, 0, true, 500., 600.));
        assert_eq!((frames[0].w, frames[1].x, frames[1].w), (600., 600., 400.));
        assert!(!move_border(&mut frames, 0, true, 0., 50.));
    }
}
