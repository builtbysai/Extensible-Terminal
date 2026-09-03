use crate::EndpointId;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SurfaceId(String);

impl SurfaceId {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.trim().is_empty()).then_some(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SurfaceState {
    Active,
    Hidden,
    Closed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Surface {
    id: SurfaceId,
    endpoint_id: EndpointId,
    state: SurfaceState,
}

impl Surface {
    #[must_use]
    pub fn new(id: SurfaceId, endpoint_id: EndpointId) -> Self {
        Self {
            id,
            endpoint_id,
            state: SurfaceState::Active,
        }
    }

    #[must_use]
    pub fn id(&self) -> &SurfaceId {
        &self.id
    }

    #[must_use]
    pub fn endpoint_id(&self) -> &EndpointId {
        &self.endpoint_id
    }

    #[must_use]
    pub fn state(&self) -> SurfaceState {
        self.state
    }

    pub fn set_state(&mut self, state: SurfaceState) {
        self.state = state;
    }

    pub fn rebind(&mut self, endpoint_id: EndpointId) {
        self.endpoint_id = endpoint_id;
    }
}
