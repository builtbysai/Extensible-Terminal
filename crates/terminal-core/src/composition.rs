use std::collections::HashSet;

use crate::SurfaceId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SplitAxis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SplitRatio(u16);

impl SplitRatio {
    #[must_use]
    pub fn new(per_mille: u16) -> Option<Self> {
        (1..1000).contains(&per_mille).then_some(Self(per_mille))
    }

    #[must_use]
    pub fn per_mille(self) -> u16 {
        self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LayoutNode {
    Surface(SurfaceId),
    Split {
        axis: SplitAxis,
        ratio: SplitRatio,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}

impl LayoutNode {
    #[must_use]
    pub fn surface(surface_id: SurfaceId) -> Self {
        Self::Surface(surface_id)
    }

    #[must_use]
    pub fn split(
        axis: SplitAxis,
        ratio: SplitRatio,
        first: LayoutNode,
        second: LayoutNode,
    ) -> Self {
        Self::Split {
            axis,
            ratio,
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    fn collect_surface_ids<'a>(&'a self, ids: &mut Vec<&'a SurfaceId>) {
        match self {
            Self::Surface(surface_id) => ids.push(surface_id),
            Self::Split { first, second, .. } => {
                first.collect_surface_ids(ids);
                second.collect_surface_ids(ids);
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Composition {
    root: LayoutNode,
    focused: SurfaceId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompositionError {
    DuplicateSurface(SurfaceId),
    UnknownSurface(SurfaceId),
    FocusedSurfaceMissing(SurfaceId),
}

impl Composition {
    /// Builds a composition whose layout references only known, unique surfaces.
    ///
    /// # Errors
    ///
    /// Returns an error when the layout references an unknown surface, repeats a surface,
    /// or when the requested focused surface is not part of the layout.
    pub fn new(
        root: LayoutNode,
        focused: SurfaceId,
        available_surfaces: impl IntoIterator<Item = SurfaceId>,
    ) -> Result<Self, CompositionError> {
        let available: HashSet<SurfaceId> = available_surfaces.into_iter().collect();
        let mut layout_ids = Vec::new();
        root.collect_surface_ids(&mut layout_ids);

        let mut seen = HashSet::new();
        for surface_id in layout_ids {
            if !available.contains(surface_id) {
                return Err(CompositionError::UnknownSurface(surface_id.clone()));
            }
            if !seen.insert(surface_id.clone()) {
                return Err(CompositionError::DuplicateSurface(surface_id.clone()));
            }
        }

        if !seen.contains(&focused) {
            return Err(CompositionError::FocusedSurfaceMissing(focused));
        }

        Ok(Self { root, focused })
    }

    #[must_use]
    pub fn root(&self) -> &LayoutNode {
        &self.root
    }

    #[must_use]
    pub fn focused(&self) -> &SurfaceId {
        &self.focused
    }

    /// Changes focus without modifying layout or endpoint policy.
    ///
    /// # Errors
    ///
    /// Returns an error when the requested surface is not part of the composition.
    pub fn set_focused(&mut self, surface_id: SurfaceId) -> Result<(), CompositionError> {
        let mut layout_ids = Vec::new();
        self.root.collect_surface_ids(&mut layout_ids);
        if layout_ids.contains(&&surface_id) {
            self.focused = surface_id;
            Ok(())
        } else {
            Err(CompositionError::FocusedSurfaceMissing(surface_id))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn surface_id(value: &str) -> SurfaceId {
        SurfaceId::new(value).expect("valid surface id")
    }

    #[test]
    fn ratio_rejects_collapsed_splits() {
        assert_eq!(SplitRatio::new(0), None);
        assert_eq!(SplitRatio::new(1000), None);
        assert_eq!(SplitRatio::new(500).map(SplitRatio::per_mille), Some(500));
    }

    #[test]
    fn composition_accepts_declarative_split_without_endpoint_policy() {
        let left = surface_id("surface:left");
        let right = surface_id("surface:right");
        let root = LayoutNode::split(
            SplitAxis::Horizontal,
            SplitRatio::new(500).unwrap(),
            LayoutNode::surface(left.clone()),
            LayoutNode::surface(right.clone()),
        );

        let composition = Composition::new(root, left.clone(), [left.clone(), right]).unwrap();
        assert_eq!(composition.focused(), &left);
    }

    #[test]
    fn composition_rejects_unknown_and_duplicate_surfaces() {
        let known = surface_id("surface:known");
        let unknown = surface_id("surface:unknown");
        assert_eq!(
            Composition::new(
                LayoutNode::surface(unknown.clone()),
                unknown.clone(),
                [known.clone()],
            ),
            Err(CompositionError::UnknownSurface(unknown))
        );

        let duplicate = LayoutNode::split(
            SplitAxis::Vertical,
            SplitRatio::new(500).unwrap(),
            LayoutNode::surface(known.clone()),
            LayoutNode::surface(known.clone()),
        );
        assert_eq!(
            Composition::new(duplicate, known.clone(), [known.clone()]),
            Err(CompositionError::DuplicateSurface(known))
        );
    }

    #[test]
    fn focus_must_remain_inside_the_composition() {
        let left = surface_id("surface:left");
        let outside = surface_id("surface:outside");
        let mut composition = Composition::new(
            LayoutNode::surface(left.clone()),
            left.clone(),
            [left.clone(), outside.clone()],
        )
        .unwrap();

        assert_eq!(
            composition.set_focused(outside.clone()),
            Err(CompositionError::FocusedSurfaceMissing(outside))
        );
        assert_eq!(composition.focused(), &left);
    }
}
