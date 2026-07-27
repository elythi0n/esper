//! The media path as one thing: capture, damage, convert, encode, and the
//! timing that makes it measurable.

pub mod pipeline;
pub mod timing;

pub use pipeline::{Pipeline, PipelineError, PipelineStats, Step, Unapplied};

// **Re-exported because they are in this crate's own signatures.** A caller
// that has to name `esper_encode::budget::Fidelity` to call
// `Pipeline::set_requested_fidelity` needs a dependency on `esper-encode` for
// no other reason — which is how a crate ends up depending on the whole
// encoder to pass it one enum.
pub use esper_encode::budget::{Budget, Fidelity, Level};
pub use timing::{Stage, Timer};
