//! Windows API wrappers. All `unsafe` code of the crate is in this module
//! tree, in small modules with a `SAFETY` comment on every block.

pub mod capture;
pub mod gfx;
pub mod ime;
pub mod pointer;
pub mod shm;
pub mod swapchain;
pub mod time;
pub mod vsync;
pub mod win;
