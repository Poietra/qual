//! Lifecycle event IR (DESIGN §5.6).
//!
//! TODO(phase-2): `Alloc`, `Alias`, `SceneAdd`, `SceneRemove`,
//! `RegisterUpdater`, `CreateAnimation`, `BeginPlay`, `FrameCallback`,
//! `FinishPlay`, `Cost`, `UnknownMutation`, ... — the shared fact stream for
//! lifecycle and cost rules.

/// Placeholder for the ordered stream of lifecycle events.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct EventStream {}
