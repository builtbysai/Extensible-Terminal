use crate::EndpointId;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AnnotationKind {
    Prompt,
    CommandStart,
    CommandFinished { exit_code: Option<i32> },
    WorkingDirectory,
    UserDefined(String),
}

/// Optional semantic metadata layered on top of terminal correctness.
///
/// Annotations can be absent, delayed, or malformed without affecting the
/// endpoint byte stream or rendering state machine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Annotation {
    pub endpoint_id: EndpointId,
    pub kind: AnnotationKind,
    pub offset: u64,
}
