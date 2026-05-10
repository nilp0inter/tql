//! Host ↔ script marshaling types.

/// Validated output of a tracker's `classify(input)` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifyOutput {
    /// `link:...` tags. Each one has already passed §5 producer validation.
    pub link_tags: Vec<String>,
    /// Informational tags. Free-form; the post-processor ignores them.
    pub info_tags: Vec<String>,
    /// Warnings the script emitted. Surfaced to the transport caller.
    pub warnings: Vec<String>,
}

/// Errors from running and validating a tracker script.
#[derive(Debug)]
pub enum ClassifyError {
    /// Script source failed to compile.
    Compile(String),
    /// Script ran but raised a Rhai error (op budget, type error, etc).
    Runtime(String),
    /// Script returned a value that wasn't a map, or whose fields had the
    /// wrong shape (link_tags/info_tags/warnings must be arrays of strings).
    BadReturnShape(String),
    /// Output array exceeded a hard cap.
    TooLarge(String),
    /// A link tag failed §5 producer-rule validation.
    InvalidLinkTag {
        tag: String,
        reason: crate::paths::LinkTagError,
    },
    /// The script produced zero link tags (§5: "at least one link tag").
    NoLinkTags,
}

impl std::fmt::Display for ClassifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClassifyError::Compile(m) => write!(f, "classify compile error: {m}"),
            ClassifyError::Runtime(m) => write!(f, "classify runtime error: {m}"),
            ClassifyError::BadReturnShape(m) => write!(f, "classify return shape: {m}"),
            ClassifyError::TooLarge(m) => write!(f, "classify output too large: {m}"),
            ClassifyError::InvalidLinkTag { tag, reason } => {
                write!(f, "invalid link tag {tag:?}: {reason:?}")
            }
            ClassifyError::NoLinkTags => write!(f, "classify produced no link tags"),
        }
    }
}

impl std::error::Error for ClassifyError {}
