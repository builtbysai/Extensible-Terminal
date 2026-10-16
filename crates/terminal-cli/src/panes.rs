//! Split panes: a binary split tree of terminal panes per tab.
//!
//! lite-style restraint applies: panes are keyboard-driven, dividers are a
//! single dim cell, and there is no pane chrome (no titles, no buttons, no
//! borders). Each leaf pane owns a full PTY endpoint and terminal state.
//!
//! The tree supports in-place surgery (split the focused pane, close it)
//! without placeholder values escaping: [`PaneNode::Vacant`] exists only
//! transiently inside [`PaneTree::split_focused`] and
//! [`PaneTree::close_focused`] and is never observable afterwards.

#![forbid(unsafe_code)]

use terminal_core::{Cell, Cursor, Endpoint, EndpointSize, TerminalState};

/// Minimum pane geometry. Splits that would produce a smaller pane are refused.
pub const MIN_PANE_COLS: u16 = 10;
pub const MIN_PANE_ROWS: u16 = 4;

/// A rectangle in content-area cells: `col`/`row` are offsets from the
/// content origin (row 0 is just below the tab bar), `cols`/`rows` its size.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rect {
    pub col: u16,
    pub row: u16,
    pub cols: u16,
    pub rows: u16,
}

/// Split direction. `Horizontal` stacks panes top/bottom (splits rows);
/// `Vertical` places them side by side (splits cols).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SplitDirection {
    Horizontal,
    Vertical,
}

/// A one-cell divider between two panes, in content-area coordinates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PaneDivider {
    pub col: u16,
    pub row: u16,
    pub cols: u16,
    pub rows: u16,
    /// True for a vertical line (side-by-side panes), false for horizontal.
    pub vertical: bool,
}

/// A pane's computed layout: which pane id goes in which rectangle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PaneLayout {
    pub id: u64,
    pub rect: Rect,
}

/// Damage-tracking render cache for one pane's terminal grid.
#[derive(Debug, Default)]
pub struct RenderCache {
    pub size: Option<EndpointSize>,
    pub cells: Vec<Cell>,
    pub cursor: Cursor,
    pub cursor_visible: bool,
    pub initialized: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RenderStats {
    pub full_redraw: bool,
    pub cells_emitted: usize,
}

/// One terminal pane: a full PTY endpoint plus its terminal state.
pub struct Pane {
    pub id: u64,
    pub endpoint: Box<dyn Endpoint>,
    pub state: TerminalState,
    pub render_cache: RenderCache,
    pub title: String,
}

impl Pane {
    pub fn new(
        id: u64,
        endpoint: Box<dyn Endpoint>,
        size: EndpointSize,
        scrollback_limit: usize,
        title: String,
    ) -> Self {
        Self {
            id,
            endpoint,
            state: TerminalState::new(size, scrollback_limit),
            render_cache: RenderCache::default(),
            title,
        }
    }

    pub fn size(&self) -> EndpointSize {
        self.state.size()
    }
}

// A pane owns a full terminal state, so the leaf variant is unavoidably
// much larger than the split variant.
#[allow(clippy::large_enum_variant)]
enum PaneNode {
    Leaf(Pane),
    Split {
        direction: SplitDirection,
        ratio: f32,
        first: Box<PaneNode>,
        second: Box<PaneNode>,
    },
    /// Transient placeholder used only during in-place tree surgery
    /// (`split_focused` / `close_focused`). It is always replaced before the
    /// operation returns and must never be observed by layout or focus code.
    Vacant,
}

/// Result of closing the focused pane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CloseResult {
    /// A pane was closed and the layout collapsed around it.
    ClosedPane,
    /// It was the last pane; the caller should close the tab instead.
    LastPane,
}

/// A binary split tree of panes with a focused leaf.
pub struct PaneTree {
    root: PaneNode,
    focused: u64,
    next_id: u64,
}

impl PaneTree {
    /// A tree with a single pane.
    pub fn new(endpoint: Box<dyn Endpoint>, size: EndpointSize, title: String) -> Self {
        let pane = Pane::new(0, endpoint, size, super::SCROLLBACK_LIMIT, title);
        Self {
            root: PaneNode::Leaf(pane),
            focused: 0,
            next_id: 1,
        }
    }

    /// Reserve an id for a pane that is about to be created.
    pub fn reserve_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn focused_id(&self) -> u64 {
        self.focused
    }

    pub fn focused_mut(&mut self) -> &mut Pane {
        self.pane_mut(self.focused)
            .expect("focused pane id always refers to a live leaf")
    }

    pub fn pane_mut(&mut self, id: u64) -> Option<&mut Pane> {
        Self::find_mut(&mut self.root, id)
    }

    fn find_mut(node: &mut PaneNode, id: u64) -> Option<&mut Pane> {
        match node {
            PaneNode::Leaf(pane) if pane.id == id => Some(pane),
            PaneNode::Leaf(_) => None,
            PaneNode::Split { first, second, .. } => {
                Self::find_mut(first, id).or_else(|| Self::find_mut(second, id))
            }
            PaneNode::Vacant => {
                debug_assert!(false, "vacant node observed outside surgery");
                None
            }
        }
    }

    /// Calls `f` for every leaf pane. Used to poll all pane endpoints.
    pub fn for_each_pane_mut(&mut self, mut f: impl FnMut(&mut Pane)) {
        Self::visit_mut(&mut self.root, &mut f);
    }

    fn visit_mut(node: &mut PaneNode, f: &mut impl FnMut(&mut Pane)) {
        match node {
            PaneNode::Leaf(pane) => f(pane),
            PaneNode::Split { first, second, .. } => {
                Self::visit_mut(first, f);
                Self::visit_mut(second, f);
            }
            PaneNode::Vacant => {
                debug_assert!(false, "vacant node observed outside surgery");
            }
        }
    }

    /// Invalidate every pane's damage cache (forces full redraws).
    pub fn invalidate_caches(&mut self) {
        self.for_each_pane_mut(|pane| pane.render_cache.initialized = false);
    }

    /// Leaf panes in layout order: depth-first, first child before second.
    /// This is the order focus cycles through.
    pub fn layout_order(&self) -> Vec<u64> {
        let mut ids = Vec::new();
        Self::collect_ids(&self.root, &mut ids);
        ids
    }

    fn collect_ids(node: &PaneNode, ids: &mut Vec<u64>) {
        match node {
            PaneNode::Leaf(pane) => ids.push(pane.id),
            PaneNode::Split { first, second, .. } => {
                Self::collect_ids(first, ids);
                Self::collect_ids(second, ids);
            }
            PaneNode::Vacant => {
                debug_assert!(false, "vacant node observed outside surgery");
            }
        }
    }

    /// Move focus to the next pane in layout order (wraps around).
    pub fn focus_next(&mut self) {
        let order = self.layout_order();
        if order.len() < 2 {
            return;
        }
        let current = order.iter().position(|&id| id == self.focused).unwrap_or(0);
        self.focused = order[(current + 1) % order.len()];
    }

    /// Move focus to the previous pane in layout order (wraps around).
    pub fn focus_prev(&mut self) {
        let order = self.layout_order();
        if order.len() < 2 {
            return;
        }
        let current = order.iter().position(|&id| id == self.focused).unwrap_or(0);
        self.focused = order[current.checked_sub(1).unwrap_or(order.len() - 1)];
    }

    /// Compute each leaf's rectangle within `area`, plus the dividers.
    pub fn compute(&self, area: Rect) -> (Vec<PaneLayout>, Vec<PaneDivider>) {
        let mut layouts = Vec::new();
        let mut dividers = Vec::new();
        Self::layout_node(&self.root, area, &mut layouts, &mut dividers);
        (layouts, dividers)
    }

    fn layout_node(
        node: &PaneNode,
        rect: Rect,
        layouts: &mut Vec<PaneLayout>,
        dividers: &mut Vec<PaneDivider>,
    ) {
        match node {
            PaneNode::Leaf(pane) => layouts.push(PaneLayout {
                id: pane.id,
                rect,
            }),
            PaneNode::Split {
                direction,
                ratio,
                first,
                second,
            } => {
                let (first_rect, second_rect, divider) = split_rect(rect, *direction, *ratio);
                dividers.push(divider);
                Self::layout_node(first, first_rect, layouts, dividers);
                Self::layout_node(second, second_rect, layouts, dividers);
            }
            PaneNode::Vacant => {
                debug_assert!(false, "vacant node observed outside surgery");
            }
        }
    }

    /// The content rectangle of the focused pane within `area`.
    pub fn focused_rect(&self, area: Rect) -> Option<Rect> {
        let (layouts, _) = self.compute(area);
        layouts
            .into_iter()
            .find(|layout| layout.id == self.focused)
            .map(|layout| layout.rect)
    }

    /// The sizes the focused pane and a new pane would get from a split,
    /// or `None` when either half would be below the minimum pane size.
    /// Callers should check this *before* spawning a PTY for the new pane.
    pub fn planned_split(
        &self,
        area: Rect,
        direction: SplitDirection,
    ) -> Option<(EndpointSize, EndpointSize)> {
        let focused_rect = self.focused_rect(area)?;
        // A split needs two minimum panes plus the one-cell divider.
        let (need_cols, need_rows) = match direction {
            SplitDirection::Vertical => (2 * MIN_PANE_COLS + 1, MIN_PANE_ROWS),
            SplitDirection::Horizontal => (MIN_PANE_COLS, 2 * MIN_PANE_ROWS + 1),
        };
        if focused_rect.cols < need_cols || focused_rect.rows < need_rows {
            return None;
        }
        let (keep, new, _) = split_rect(focused_rect, direction, 0.5);
        // Rects are non-empty by construction, so these cannot fail.
        let keep_size = EndpointSize::new(keep.cols, keep.rows).ok()?;
        let new_size = EndpointSize::new(new.cols, new.rows).ok()?;
        Some((keep_size, new_size))
    }

    /// Split the focused pane. The focused (kept) pane becomes `first`,
    /// the new pane becomes `second`, and focus moves to the new pane.
    /// The caller is responsible for resizing the kept pane to its new
    /// geometry (see [`PaneTree::planned_split`]).
    pub fn split_focused(&mut self, direction: SplitDirection, new_pane: Pane) {
        let new_id = new_pane.id;
        let mut slot = Some(new_pane);
        Self::split_node(&mut self.root, self.focused, direction, &mut slot);
        debug_assert!(slot.is_none(), "focused pane must exist exactly once");
        self.focused = new_id;
    }

    fn split_node(
        node: &mut PaneNode,
        focused: u64,
        direction: SplitDirection,
        slot: &mut Option<Pane>,
    ) {
        match node {
            PaneNode::Leaf(pane) if pane.id == focused => {
                let new_pane = slot.take().expect("focused pane exists exactly once");
                let old_pane = std::mem::replace(pane, new_pane);
                // *node is now Leaf(new pane). Build the split around it:
                // swap in a skeleton, then plug the new pane into `second`.
                let mut skeleton = PaneNode::Split {
                    direction,
                    ratio: 0.5,
                    first: Box::new(PaneNode::Leaf(old_pane)),
                    second: Box::new(PaneNode::Vacant),
                };
                std::mem::swap(node, &mut skeleton);
                // `skeleton` now holds Leaf(new pane); `node` holds the split.
                if let PaneNode::Leaf(new_pane) = skeleton {
                    if let PaneNode::Split { second, .. } = node {
                        **second = PaneNode::Leaf(new_pane);
                    } else {
                        unreachable!("node just swapped to a split");
                    }
                } else {
                    unreachable!("skeleton held the leaf we swapped out");
                }
            }
            PaneNode::Leaf(_) => {}
            PaneNode::Split { first, second, .. } => {
                Self::split_node(first, focused, direction, slot);
                Self::split_node(second, focused, direction, slot);
            }
            PaneNode::Vacant => {
                debug_assert!(false, "vacant node observed outside surgery");
            }
        }
    }

    /// Close the pane with `id`, collapsing the layout around it.
    /// Unlike [`PaneTree::close_focused`], this preserves the user's focus
    /// when the closed pane was not focused: focus returns to the
    /// previously focused pane when it survives the surgery.
    pub fn close_pane(&mut self, id: u64) -> CloseResult {
        if !self.layout_order().contains(&id) {
            return self.close_focused();
        }
        let previous_focus = self.focused;
        self.focused = id;
        let result = self.close_focused();
        if result == CloseResult::ClosedPane
            && previous_focus != id
            && self.layout_order().contains(&previous_focus)
        {
            self.focused = previous_focus;
        }
        result
    }

    /// Close the focused pane, collapsing the layout around it.
    /// Focus moves to the first pane (in layout order) of the subtree that
    /// takes the closed pane's place.
    pub fn close_focused(&mut self) -> CloseResult {
        if matches!(self.root, PaneNode::Leaf(_)) {
            return CloseResult::LastPane;
        }
        let focused = self.focused;
        let mut new_focus = None;
        let removed = Self::close_node(&mut self.root, focused, &mut new_focus);
        debug_assert!(removed, "focused pane must exist");
        self.focused = new_focus.expect("closing a pane leaves a survivor");
        // The tree can never be left vacant: close_node only promotes real subtrees.
        debug_assert!(!matches!(self.root, PaneNode::Vacant));
        CloseResult::ClosedPane
    }

    /// Returns true when the focused leaf was removed within this subtree.
    fn close_node(node: &mut PaneNode, focused: u64, new_focus: &mut Option<u64>) -> bool {
        match node {
            PaneNode::Leaf(pane) => pane.id == focused,
            PaneNode::Split { first, second, .. } => {
                let first_is_target = matches!(**first, PaneNode::Leaf(ref pane) if pane.id == focused);
                let second_is_target = matches!(**second, PaneNode::Leaf(ref pane) if pane.id == focused);
                if first_is_target || second_is_target {
                    // Promote the surviving sibling into this node's position.
                    let survivor: &mut Box<PaneNode> = if first_is_target { second } else { first };
                    let mut promoted = PaneNode::Vacant;
                    std::mem::swap(&mut **survivor, &mut promoted);
                    // `promoted` now holds the sibling subtree; the old split
                    // (target leaf + vacant box) is swapped out and dropped,
                    // which also drops the closed pane's endpoint.
                    std::mem::swap(node, &mut promoted);
                    drop(promoted);
                    if new_focus.is_none() {
                        *new_focus = Some(Self::first_leaf_id(node));
                    }
                    return true;
                }
                if Self::close_node(first, focused, new_focus) {
                    return true;
                }
                Self::close_node(second, focused, new_focus)
            }
            PaneNode::Vacant => {
                debug_assert!(false, "vacant node observed outside surgery");
                false
            }
        }
    }

    fn first_leaf_id(node: &PaneNode) -> u64 {
        match node {
            PaneNode::Leaf(pane) => pane.id,
            PaneNode::Split { first, .. } => Self::first_leaf_id(first),
            PaneNode::Vacant => {
                debug_assert!(false, "vacant node observed outside surgery");
                u64::MAX
            }
        }
    }
}

/// Split `rect` into two panes with a one-cell divider between them,
/// giving `ratio` of the space to the first pane.
///
/// Never panics: when the rect is too small for two minimum panes (the host
/// was shrunk to an absurd size), it splits evenly instead. The layout looks
/// broken until the host grows again, but nothing crashes.
fn split_rect(rect: Rect, direction: SplitDirection, ratio: f32) -> (Rect, Rect, PaneDivider) {
    match direction {
        SplitDirection::Vertical => {
            let inner = rect.cols.saturating_sub(1);
            let first_cols = if inner >= 2 * MIN_PANE_COLS {
                // Bounded: inner <= u16::MAX and ratio <= 1.0, so the
                // rounded product cannot exceed inner.
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let scaled = (f32::from(inner) * ratio).round() as u16;
                scaled.clamp(MIN_PANE_COLS, inner - MIN_PANE_COLS)
            } else {
                inner / 2
            };
            let first = Rect {
                col: rect.col,
                row: rect.row,
                cols: first_cols,
                rows: rect.rows,
            };
            let divider = PaneDivider {
                col: rect.col + first_cols,
                row: rect.row,
                cols: 1,
                rows: rect.rows,
                vertical: true,
            };
            let second = Rect {
                col: rect.col + first_cols + 1,
                row: rect.row,
                cols: rect.cols.saturating_sub(first_cols + 1),
                rows: rect.rows,
            };
            (first, second, divider)
        }
        SplitDirection::Horizontal => {
            let inner = rect.rows.saturating_sub(1);
            let first_rows = if inner >= 2 * MIN_PANE_ROWS {
                // Bounded: inner <= u16::MAX and ratio <= 1.0, so the
                // rounded product cannot exceed inner.
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let scaled = (f32::from(inner) * ratio).round() as u16;
                scaled.clamp(MIN_PANE_ROWS, inner - MIN_PANE_ROWS)
            } else {
                inner / 2
            };
            let first = Rect {
                col: rect.col,
                row: rect.row,
                cols: rect.cols,
                rows: first_rows,
            };
            let divider = PaneDivider {
                col: rect.col,
                row: rect.row + first_rows,
                cols: rect.cols,
                rows: 1,
                vertical: false,
            };
            let second = Rect {
                col: rect.col,
                row: rect.row + first_rows + 1,
                cols: rect.cols,
                rows: rect.rows.saturating_sub(first_rows + 1),
            };
            (first, second, divider)
        }
    }
}

#[cfg(test)]
#[allow(clippy::assert_is_empty)]
mod tests {
    use super::*;
    use terminal_core::{
        EndpointCapabilities, EndpointError, EndpointEvent, EndpointId, EndpointKind,
        EndpointLocality,
    };

    /// Minimal in-memory endpoint so pane-tree logic can be tested without PTYs.
    struct StubEndpoint {
        id: EndpointId,
        size: EndpointSize,
        locality: EndpointLocality,
    }

    impl StubEndpoint {
        fn boxed(name: &str, size: EndpointSize) -> Box<dyn Endpoint> {
            Box::new(Self {
                id: EndpointId::new(name).unwrap(),
                size,
                locality: EndpointLocality::Local,
            })
        }
    }

    impl Endpoint for StubEndpoint {
        fn id(&self) -> &EndpointId {
            &self.id
        }
        fn kind(&self) -> EndpointKind {
            EndpointKind::LocalPty
        }
        fn locality(&self) -> &EndpointLocality {
            &self.locality
        }
        fn capabilities(&self) -> EndpointCapabilities {
            EndpointCapabilities::default()
        }
        fn attach(&mut self) -> Result<(), EndpointError> {
            Ok(())
        }
        fn detach(&mut self) -> Result<(), EndpointError> {
            Ok(())
        }
        fn resize(&mut self, size: EndpointSize) -> Result<(), EndpointError> {
            self.size = size;
            Ok(())
        }
        fn send(&mut self, _input: &[u8]) -> Result<(), EndpointError> {
            Ok(())
        }
        fn poll_event(&mut self) -> Result<Option<EndpointEvent>, EndpointError> {
            Ok(None)
        }
    }

    fn tree() -> PaneTree {
        let size = EndpointSize::new(80, 24).unwrap();
        PaneTree::new(StubEndpoint::boxed("stub:0", size), size, "t".to_string())
    }

    fn area() -> Rect {
        Rect {
            col: 0,
            row: 0,
            cols: 80,
            rows: 24,
        }
    }

    fn split_tree(direction: SplitDirection) -> PaneTree {
        let mut tree = tree();
        let size = EndpointSize::new(40, 24).unwrap();
        let id = tree.reserve_id();
        tree.split_focused(
            direction,
            Pane::new(id, StubEndpoint::boxed("stub:1", size), size, 0, "t".to_string()),
        );
        tree
    }

    #[test]
    fn single_pane_fills_area_with_no_dividers() {
        let tree = tree();
        let (layouts, dividers) = tree.compute(area());
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].rect, area());
        assert!(dividers.is_empty());
        assert_eq!(tree.layout_order().len(), 1);
    }

    #[test]
    fn vertical_split_tiles_side_by_side_with_one_divider() {
        let tree = split_tree(SplitDirection::Vertical);
        let (layouts, dividers) = tree.compute(area());
        assert_eq!(layouts.len(), 2);
        assert_eq!(dividers.len(), 1);

        let first = layouts[0].rect;
        let second = layouts[1].rect;
        let divider = dividers[0];
        assert!(divider.vertical);
        // First pane + 1-cell divider + second pane == full width.
        assert_eq!(first.cols + 1 + second.cols, 80);
        assert_eq!(divider.col, first.cols);
        assert_eq!(second.col, first.cols + 1);
        assert_eq!(first.rows, 24);
        assert_eq!(second.rows, 24);
        // Roughly even split.
        assert!(first.cols.abs_diff(second.cols) <= 1);
    }

    #[test]
    fn horizontal_split_stacks_with_one_divider() {
        let tree = split_tree(SplitDirection::Horizontal);
        let (layouts, dividers) = tree.compute(area());
        assert_eq!(layouts.len(), 2);
        assert_eq!(dividers.len(), 1);

        let first = layouts[0].rect;
        let second = layouts[1].rect;
        let divider = dividers[0];
        assert!(!divider.vertical);
        assert_eq!(first.rows + 1 + second.rows, 24);
        assert_eq!(divider.row, first.rows);
        assert_eq!(second.row, first.rows + 1);
        assert_eq!(first.cols, 80);
        assert_eq!(second.cols, 80);
    }

    #[test]
    fn nested_splits_tile_without_overlap() {
        let mut tree = split_tree(SplitDirection::Vertical);
        // Split the focused (second/right) pane horizontally.
        let size = EndpointSize::new(40, 12).unwrap();
        let id = tree.reserve_id();
        tree.split_focused(
            SplitDirection::Horizontal,
            Pane::new(id, StubEndpoint::boxed("stub:2", size), size, 0, "t".to_string()),
        );
        assert_eq!(tree.layout_order().len(), 3);

        let (layouts, dividers) = tree.compute(area());
        assert_eq!(layouts.len(), 3);
        assert_eq!(dividers.len(), 2);
        // No two pane rects overlap.
        for (i, a) in layouts.iter().enumerate() {
            for b in layouts.iter().skip(i + 1) {
                let overlap = a.rect.col < b.rect.col + b.rect.cols
                    && b.rect.col < a.rect.col + a.rect.cols
                    && a.rect.row < b.rect.row + b.rect.rows
                    && b.rect.row < a.rect.row + a.rect.rows;
                assert!(!overlap, "panes {a:?} and {b:?} overlap");
            }
        }
    }

    #[test]
    fn split_moves_focus_to_new_pane() {
        let tree = split_tree(SplitDirection::Vertical);
        assert_eq!(tree.focused_id(), 1);
        assert_eq!(tree.layout_order(), vec![0, 1]);
    }

    #[test]
    fn focus_cycles_in_layout_order() {
        let mut tree = split_tree(SplitDirection::Vertical);
        tree.focus_next();
        assert_eq!(tree.focused_id(), 0);
        tree.focus_next();
        assert_eq!(tree.focused_id(), 1);
        tree.focus_prev();
        assert_eq!(tree.focused_id(), 0);
    }

    #[test]
    fn close_collapses_layout_and_moves_focus_to_sibling() {
        let mut tree = split_tree(SplitDirection::Vertical);
        // Focus is on pane 1 (right). Close it.
        assert_eq!(tree.close_focused(), CloseResult::ClosedPane);
        assert_eq!(tree.layout_order().len(), 1);
        assert_eq!(tree.focused_id(), 0);
        let (layouts, dividers) = tree.compute(area());
        assert_eq!(layouts.len(), 1);
        assert_eq!(layouts[0].rect, area());
        assert!(dividers.is_empty());
    }

    #[test]
    fn close_deep_leaf_promotes_sibling_subtree() {
        let mut tree = split_tree(SplitDirection::Vertical);
        let size = EndpointSize::new(40, 12).unwrap();
        let id = tree.reserve_id();
        tree.split_focused(
            SplitDirection::Horizontal,
            Pane::new(id, StubEndpoint::boxed("stub:2", size), size, 0, "t".to_string()),
        );
        // Panes: 0 (left), 1 (top-right), 2 (bottom-right, focused).
        assert_eq!(tree.layout_order(), vec![0, 1, 2]);
        // Focus pane 1 and close it: the horizontal split collapses to pane 2.
        tree.focus_prev();
        assert_eq!(tree.focused_id(), 1);
        assert_eq!(tree.close_focused(), CloseResult::ClosedPane);
        assert_eq!(tree.layout_order().len(), 2);
        assert_eq!(tree.layout_order(), vec![0, 2]);
        assert_eq!(tree.focused_id(), 2);
    }

    #[test]
    fn close_last_pane_reports_last_pane() {
        let mut tree = tree();
        assert_eq!(tree.close_focused(), CloseResult::LastPane);
        assert_eq!(tree.layout_order().len(), 1);
    }

    #[test]
    fn planned_split_refuses_tiny_panes() {
        let tree = tree();
        // Plenty of room at 80x24.
        assert!(tree.planned_split(area(), SplitDirection::Vertical).is_some());
        assert!(tree.planned_split(area(), SplitDirection::Horizontal).is_some());
        // Too narrow / too short.
        let narrow = Rect {
            col: 0,
            row: 0,
            cols: 15,
            rows: 24,
        };
        assert!(tree.planned_split(narrow, SplitDirection::Vertical).is_none());
        let short = Rect {
            col: 0,
            row: 0,
            cols: 80,
            rows: 5,
        };
        assert!(tree.planned_split(short, SplitDirection::Horizontal).is_none());
    }

    #[test]
    fn planned_split_sizes_sum_to_focused_rect_minus_divider() {
        let tree = tree();
        let (keep, new) = tree
            .planned_split(area(), SplitDirection::Vertical)
            .unwrap();
        assert_eq!(keep.cols + 1 + new.cols, 80);
        assert_eq!(keep.rows, 24);
        assert_eq!(new.rows, 24);
        let (keep, new) = tree
            .planned_split(area(), SplitDirection::Horizontal)
            .unwrap();
        assert_eq!(keep.rows + 1 + new.rows, 24);
        assert_eq!(keep.cols, 80);
    }

    #[test]
    fn focused_rect_tracks_focus() {
        let mut tree = split_tree(SplitDirection::Vertical);
        let (layouts, _) = tree.compute(area());
        let right = layouts.iter().find(|l| l.id == 1).unwrap().rect;
        assert_eq!(tree.focused_rect(area()), Some(right));
        tree.focus_prev();
        let left = layouts.iter().find(|l| l.id == 0).unwrap().rect;
        assert_eq!(tree.focused_rect(area()), Some(left));
    }

    #[test]
    fn for_each_pane_mut_visits_every_pane() {
        let mut tree = split_tree(SplitDirection::Vertical);
        let mut ids = Vec::new();
        tree.for_each_pane_mut(|pane| ids.push(pane.id));
        ids.sort_unstable();
        assert_eq!(ids, vec![0, 1]);
    }

    #[test]
    fn degenerate_tiny_rects_never_panic() {
        let tree = tree();
        // Shrink the host to something absurd with a split active.
        let tiny = Rect {
            col: 0,
            row: 0,
            cols: 5,
            rows: 3,
        };
        let (layouts, dividers) = tree.compute(tiny);
        assert_eq!(layouts.len(), 1);
        assert!(dividers.is_empty());

        // Split, then shrink: layout must not panic even when panes are tiny.
        let split = split_tree(SplitDirection::Vertical);
        let (layouts, dividers) = split.compute(tiny);
        assert_eq!(layouts.len(), 2);
        assert_eq!(dividers.len(), 1);
        let (layouts, _) = split.compute(Rect {
            col: 0,
            row: 0,
            cols: 80,
            rows: 2,
        });
        assert_eq!(layouts.len(), 2);
    }
}
