//! The compositor clock and per-display timing.

use windows::Win32::Devices::Display::*;
use windows::Win32::Foundation::{ERROR_SUCCESS, LUID, WAIT_TIMEOUT};
use windows::Win32::Graphics::DirectComposition::*;
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, HMONITOR, MONITORINFOEXW};

/// Waits for the next tick of the compositor clock. Returns false on
/// timeout.
pub fn wait_for_compositor_clock(timeout_ms: u32) -> bool {
    // SAFETY: no handles are passed.
    let r = unsafe { DCompositionWaitForCompositorClock(None, timeout_ms) };
    r != WAIT_TIMEOUT.0
}

/// A display target's identity: adapter LUID and video present source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TargetId {
    pub adapter: i64,
    pub source: u32,
}

fn luid(l: LUID) -> i64 {
    ((l.HighPart as i64) << 32) | l.LowPart as i64
}

/// Timing of one compositor frame (performance counter units).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameTiming {
    pub frame_id: u64,
    pub start: i64,
    pub target: i64,
    pub period: i64,
}

/// Per-display timing of a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TargetTiming {
    pub id: TargetId,
    /// Refresh period of this display (performance counter units).
    pub vblank: i64,
    pub present_time: i64,
}

/// The latest frame the compositor created, with the timing of each
/// display it composed for.
pub fn latest_frame() -> Option<(FrameTiming, Vec<TargetTiming>)> {
    // SAFETY: out pointers are valid and sized as passed.
    unsafe {
        let id = DCompositionGetFrameId(COMPOSITION_FRAME_ID_CREATED).ok()?;
        let mut stats = COMPOSITION_FRAME_STATS::default();
        let mut targets = [COMPOSITION_TARGET_ID::default(); 16];
        let mut n = 0u32;
        DCompositionGetStatistics(
            id,
            &mut stats,
            targets.len() as u32,
            Some(targets.as_mut_ptr()),
            Some(&mut n),
        )
        .ok()?;
        let mut out = Vec::new();
        for t in &targets[..(n as usize).min(targets.len())] {
            if let Ok(s) = DCompositionGetTargetStatistics(id, t) {
                out.push(TargetTiming {
                    id: TargetId {
                        adapter: luid(t.displayAdapterLuid),
                        source: t.vidPnSourceId,
                    },
                    vblank: s.vblankDuration as i64,
                    present_time: s.presentTime as i64,
                });
            }
        }
        Some((
            FrameTiming {
                frame_id: id,
                start: stats.startTime as i64,
                target: stats.targetTime as i64,
                period: stats.framePeriod as i64,
            },
            out,
        ))
    }
}

/// The display target of a monitor, to pick its timing out of a frame.
pub fn monitor_target(monitor: isize) -> Option<TargetId> {
    // SAFETY: out pointers are valid and sized as passed; the buffers
    // are sized by GetDisplayConfigBufferSizes.
    unsafe {
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        if !GetMonitorInfoW(HMONITOR(monitor as *mut _), &mut info.monitorInfo).as_bool() {
            return None;
        }
        let (mut np, mut nm) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm) != ERROR_SUCCESS {
            return None;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        if QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut np,
            paths.as_mut_ptr(),
            &mut nm,
            modes.as_mut_ptr(),
            None,
        ) != ERROR_SUCCESS
        {
            return None;
        }
        for p in &paths[..np as usize] {
            let mut name = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
            name.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            name.header.size = size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
            name.header.adapterId = p.sourceInfo.adapterId;
            name.header.id = p.sourceInfo.id;
            if DisplayConfigGetDeviceInfo(&mut name.header) != 0 {
                continue;
            }
            if name.viewGdiDeviceName == info.szDevice {
                return Some(TargetId {
                    adapter: luid(p.sourceInfo.adapterId),
                    source: p.sourceInfo.id,
                });
            }
        }
        None
    }
}
