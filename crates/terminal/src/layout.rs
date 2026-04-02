//! Layout tree — manages a set of terminal panes arranged in a binary split
//! tree over the screen area.

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

use crate::pane::Pane;
use crate::render::{self, Color};
use crate::{LayoutNode, SplitDirection, Viewport};

/// Thickness of the visual separator between split panes, in pixels.
const SEPARATOR_PX: usize = 2;

/// Manages the pane layout tree and the collection of live panes.
pub struct Layout {
    /// Root of the binary split tree.
    root: LayoutNode,
    /// All panes, keyed by position in this Vec (pane id == index is NOT
    /// guaranteed after removals; use `Pane::id` for the stable identifier).
    panes: Vec<Pane>,
    /// Index into `panes` of the currently focused pane.
    focused_idx: usize,
    /// Monotonically increasing pane-id counter.
    next_pane_id: usize,
}

impl Layout {
    /// Create a layout with a single pane filling the entire screen.
    pub fn new(screen_width: usize, screen_height: usize) -> Self {
        let viewport = Viewport {
            x: 0,
            y: 0,
            width: screen_width,
            height: screen_height,
        };
        let pane = Pane::new(0, viewport.clone());
        Self {
            root: LayoutNode::Leaf {
                pane_id: 0,
                viewport,
            },
            panes: vec![pane],
            focused_idx: 0,
            next_pane_id: 1,
        }
    }

    // -- accessors ----------------------------------------------------------

    /// Reference to the currently focused pane.
    pub fn focused_pane(&self) -> &Pane {
        &self.panes[self.focused_idx]
    }

    /// Mutable reference to the currently focused pane.
    pub fn focused_pane_mut(&mut self) -> &mut Pane {
        &mut self.panes[self.focused_idx]
    }

    /// Number of panes.
    pub fn pane_count(&self) -> usize {
        self.panes.len()
    }

    /// Iterate over all panes.
    pub fn panes(&self) -> &[Pane] {
        &self.panes
    }

    /// Mutable access to a pane by its stable id, if it exists.
    pub fn pane_by_id_mut(&mut self, id: usize) -> Option<&mut Pane> {
        self.panes.iter_mut().find(|p| p.id == id)
    }

    /// The stable id of the focused pane.
    pub fn focused_pane_id(&self) -> usize {
        self.panes[self.focused_idx].id
    }

    // -- focus navigation ---------------------------------------------------

    /// Move focus to the next pane (wraps around).
    pub fn focus_next(&mut self) {
        if !self.panes.is_empty() {
            self.focused_idx = (self.focused_idx + 1) % self.panes.len();
        }
    }

    /// Move focus to the previous pane (wraps around).
    pub fn focus_prev(&mut self) {
        if !self.panes.is_empty() {
            if self.focused_idx == 0 {
                self.focused_idx = self.panes.len() - 1;
            } else {
                self.focused_idx -= 1;
            }
        }
    }

    // -- split / close ------------------------------------------------------

    /// Split the focused pane in the given direction.
    ///
    /// The focused pane keeps the first half; a new pane is created in the
    /// second half. Focus moves to the new pane.
    pub fn split(&mut self, direction: SplitDirection) {
        let focused_id = self.panes[self.focused_idx].id;
        let new_id = self.next_pane_id;
        self.next_pane_id += 1;

        // Walk the layout tree, find the leaf with `focused_id`, and replace
        // it with a Split node containing two new Leaf children.
        Self::split_node(&mut self.root, focused_id, new_id, direction);

        // Recompute viewports from the root so every node gets correct sizes.
        let root_vp = root_viewport(&self.root);
        Self::recompute_viewports(&mut self.root, root_vp);

        // Resize the existing focused pane to its new (smaller) viewport.
        if let Some(vp) = Self::viewport_for(&self.root, focused_id) {
            if let Some(pane) = self.panes.iter_mut().find(|p| p.id == focused_id) {
                pane.resize(vp);
            }
        }

        // Create the new pane with its computed viewport.
        if let Some(vp) = Self::viewport_for(&self.root, new_id) {
            let new_pane = Pane::new(new_id, vp);
            self.panes.push(new_pane);
            self.focused_idx = self.panes.len() - 1;
        }
    }

    /// Close the focused pane. If it is the last pane, this is a no-op.
    pub fn close_focused(&mut self) {
        if self.panes.len() <= 1 {
            return;
        }

        let focused_id = self.panes[self.focused_idx].id;

        // Remove pane from vec.
        self.panes.retain(|p| p.id != focused_id);

        // Remove the leaf from the layout tree (promote sibling).
        Self::remove_leaf(&mut self.root, focused_id);

        // Recompute viewports.
        let root_vp = root_viewport(&self.root);
        Self::recompute_viewports(&mut self.root, root_vp);

        // Resize remaining panes.
        for pane in &mut self.panes {
            if let Some(vp) = Self::viewport_for(&self.root, pane.id) {
                pane.resize(vp);
            }
        }

        // Fix focus index.
        if self.focused_idx >= self.panes.len() {
            self.focused_idx = self.panes.len().saturating_sub(1);
        }
    }

    // -- rendering ----------------------------------------------------------

    /// Render all panes and separators onto the draw target (full redraw).
    pub fn render_all<D: super::DrawTarget>(&self, target: &mut D) {
        for (i, pane) in self.panes.iter().enumerate() {
            pane.render(target);
            if i == self.focused_idx {
                pane.render_cursor(target);
            }
        }
        // Draw separators.
        self.render_separators(target, &self.root);
    }

    /// Render only dirty rows of dirty panes, plus cursor delta on the
    /// focused pane. This is the fast path for typing — typically re-renders
    /// only 1 character row (~16 pixel-rows) instead of the full screen.
    ///
    /// Returns `true` if anything was rendered (caller may need to
    /// blit the back buffer to the front buffer).
    pub fn render_dirty<D: super::DrawTarget>(&mut self, target: &mut D) -> bool {
        let mut rendered = false;
        for (i, pane) in self.panes.iter_mut().enumerate() {
            if pane.is_dirty() {
                pane.render_dirty(target);
                pane.clear_dirty();
                rendered = true;
            }
            if i == self.focused_idx {
                pane.render_cursor_delta(target);
                rendered = true;
            }
        }
        rendered
    }

    /// Full render of all panes, then clear all dirty flags.
    pub fn render_all_and_clear<D: super::DrawTarget>(&mut self, target: &mut D) {
        for (i, pane) in self.panes.iter_mut().enumerate() {
            pane.render(target);
            if i == self.focused_idx {
                pane.render_cursor_delta(target);
            }
            pane.clear_dirty();
        }
        self.render_separators(target, &self.root);
    }

    /// Draw separator lines between split children.
    fn render_separators<D: super::DrawTarget>(&self, target: &mut D, node: &LayoutNode) {
        match node {
            LayoutNode::Leaf { .. } => {}
            LayoutNode::Split {
                direction,
                first,
                second,
                ..
            } => {
                // Draw a line between the two children.
                let vp1 = root_viewport(first);
                let color = Color::BRIGHT_BLACK;

                match direction {
                    SplitDirection::Vertical => {
                        // Vertical split: separator is a vertical line at the
                        // right edge of the first child.
                        let sx = vp1.x + vp1.width;
                        let sy = vp1.y;
                        let sh = vp1.height;
                        render::fill_rect(target, sx, sy, SEPARATOR_PX, sh, color);
                    }
                    SplitDirection::Horizontal => {
                        // Horizontal split: separator is a horizontal line at
                        // the bottom edge of the first child.
                        let sx = vp1.x;
                        let sy = vp1.y + vp1.height;
                        let sw = vp1.width;
                        render::fill_rect(target, sx, sy, sw, SEPARATOR_PX, color);
                    }
                }

                self.render_separators(target, first);
                self.render_separators(target, second);
            }
        }
    }

    // -- tree helpers (static) ----------------------------------------------

    /// Replace the leaf with `target_id` with a Split containing the old leaf
    /// and a new leaf with `new_id`.
    fn split_node(
        node: &mut LayoutNode,
        target_id: usize,
        new_id: usize,
        direction: SplitDirection,
    ) -> bool {
        match node {
            LayoutNode::Leaf { pane_id, viewport } if *pane_id == target_id => {
                let old_vp = viewport.clone();
                // Placeholder viewports — will be fixed up by recompute_viewports.
                let first = Box::new(LayoutNode::Leaf {
                    pane_id: target_id,
                    viewport: old_vp.clone(),
                });
                let second = Box::new(LayoutNode::Leaf {
                    pane_id: new_id,
                    viewport: old_vp.clone(),
                });
                *node = LayoutNode::Split {
                    direction,
                    ratio: 0.5,
                    first,
                    second,
                };
                true
            }
            LayoutNode::Split {
                first, second, ..
            } => {
                Self::split_node(first, target_id, new_id, direction)
                    || Self::split_node(second, target_id, new_id, direction)
            }
            _ => false,
        }
    }

    /// Remove the leaf with `target_id`, promoting its sibling to take the
    /// parent split's place.
    fn remove_leaf(node: &mut LayoutNode, target_id: usize) -> bool {
        match node {
            LayoutNode::Leaf { .. } => false,
            LayoutNode::Split {
                first, second, ..
            } => {
                // Check if one of the direct children is the target leaf.
                let first_is_target = matches!(
                    first.as_ref(),
                    LayoutNode::Leaf { pane_id, .. } if *pane_id == target_id
                );
                let second_is_target = matches!(
                    second.as_ref(),
                    LayoutNode::Leaf { pane_id, .. } if *pane_id == target_id
                );

                if first_is_target {
                    // Promote second child.
                    let promoted = core::mem::replace(
                        second.as_mut(),
                        LayoutNode::Leaf {
                            pane_id: 0,
                            viewport: Viewport {
                                x: 0,
                                y: 0,
                                width: 0,
                                height: 0,
                            },
                        },
                    );
                    *node = promoted;
                    true
                } else if second_is_target {
                    let promoted = core::mem::replace(
                        first.as_mut(),
                        LayoutNode::Leaf {
                            pane_id: 0,
                            viewport: Viewport {
                                x: 0,
                                y: 0,
                                width: 0,
                                height: 0,
                            },
                        },
                    );
                    *node = promoted;
                    true
                } else {
                    // Recurse.
                    Self::remove_leaf(first, target_id)
                        || Self::remove_leaf(second, target_id)
                }
            }
        }
    }

    /// Walk the tree and assign correct viewports based on parent viewport and
    /// split ratios.
    fn recompute_viewports(node: &mut LayoutNode, vp: Viewport) {
        match node {
            LayoutNode::Leaf { viewport, .. } => {
                *viewport = vp;
            }
            LayoutNode::Split {
                direction,
                ratio,
                first,
                second,
            } => match direction {
                SplitDirection::Vertical => {
                    let first_w =
                        ((vp.width as f32 * *ratio) as usize).saturating_sub(SEPARATOR_PX / 2);
                    let second_x = vp.x + first_w + SEPARATOR_PX;
                    let second_w = vp.width.saturating_sub(first_w + SEPARATOR_PX);

                    let vp1 = Viewport {
                        x: vp.x,
                        y: vp.y,
                        width: first_w,
                        height: vp.height,
                    };
                    let vp2 = Viewport {
                        x: second_x,
                        y: vp.y,
                        width: second_w,
                        height: vp.height,
                    };
                    Self::recompute_viewports(first, vp1);
                    Self::recompute_viewports(second, vp2);
                }
                SplitDirection::Horizontal => {
                    let first_h =
                        ((vp.height as f32 * *ratio) as usize).saturating_sub(SEPARATOR_PX / 2);
                    let second_y = vp.y + first_h + SEPARATOR_PX;
                    let second_h = vp.height.saturating_sub(first_h + SEPARATOR_PX);

                    let vp1 = Viewport {
                        x: vp.x,
                        y: vp.y,
                        width: vp.width,
                        height: first_h,
                    };
                    let vp2 = Viewport {
                        x: vp.x,
                        y: second_y,
                        width: vp.width,
                        height: second_h,
                    };
                    Self::recompute_viewports(first, vp1);
                    Self::recompute_viewports(second, vp2);
                }
            },
        }
    }

    /// Find the viewport assigned to a given pane id.
    fn viewport_for(node: &LayoutNode, target_id: usize) -> Option<Viewport> {
        match node {
            LayoutNode::Leaf { pane_id, viewport } if *pane_id == target_id => {
                Some(viewport.clone())
            }
            LayoutNode::Split {
                first, second, ..
            } => Self::viewport_for(first, target_id)
                .or_else(|| Self::viewport_for(second, target_id)),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// free helpers
// ---------------------------------------------------------------------------

/// Extract the bounding viewport of a node (the viewport of the outermost
/// region it covers).
fn root_viewport(node: &LayoutNode) -> Viewport {
    match node {
        LayoutNode::Leaf { viewport, .. } => viewport.clone(),
        LayoutNode::Split {
            direction,
            first,
            second,
            ..
        } => {
            let vp1 = root_viewport(first);
            let vp2 = root_viewport(second);
            match direction {
                SplitDirection::Vertical => Viewport {
                    x: vp1.x,
                    y: vp1.y,
                    width: (vp2.x + vp2.width).saturating_sub(vp1.x),
                    height: vp1.height.max(vp2.height),
                },
                SplitDirection::Horizontal => Viewport {
                    x: vp1.x,
                    y: vp1.y,
                    width: vp1.width.max(vp2.width),
                    height: (vp2.y + vp2.height).saturating_sub(vp1.y),
                },
            }
        }
    }
}
