//! `andruna-compositor`: what SurfaceFlinger and WindowManager do on a
//! device, on Windows.
//!
//! - [`compositor::Compositor`]: one thread owning the task windows
//!   (one Windows window per Android task) and their DirectComposition
//!   trees; commands in, [`compositor::Event`]s out.
//! - [`config`]: the configuration an Android window gets from its
//!   Windows window (pure functions).
//! - [`input`]: pointer and key translation to Android events (pure state
//!   machines), key character maps from Windows layouts.
//! - [`surface`]: the producer side of a surface, the interface the GLES
//!   track builds `ANativeWindow` on.
//! - [`vsync`]: vsync events from the compositor clock.
//! - [`wire`] and [`channel`]: the input message format and transport to
//!   app processes until Binder's `InputChannel`.
//! - [`producer`]: the app process's end of a window channel (surfaces
//!   handed over by the compositor, configuration, input); with the
//!   `gles` feature its surfaces are EGL window targets.
//!
//! All `unsafe` code is in [`sys`].

pub mod android;
pub mod channel;
pub mod clock;
pub mod compositor;
pub mod config;
pub mod geom;
pub mod input;
pub mod producer;
pub mod surface;
pub mod sys;
pub mod vsync;
pub mod wire;
