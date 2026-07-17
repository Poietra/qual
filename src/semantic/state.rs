//! Abstract state records (DESIGN §5.5).
//!
//! TODO(phase-2): `MobjectState`, `AnimationState`, `SceneState`,
//! `CameraState`, `OutputState`, `ResourceState` with membership, updater,
//! visibility, and renderer facts kept as separate dimensions (never one
//! boolean).

/// Placeholder for the interpreter's combined abstract state.
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct AbstractState {}
