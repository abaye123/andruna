//! The serialized form of input events between the compositor and an app
//! process.
//!
//! Until Binder and `InputChannel` exist, the compositor delivers events
//! through a [`channel`](crate::channel) as messages in this format. It
//! carries what Android's `InputMessage` carries (all `MotionEvent` and
//! `KeyEvent` fields, historical samples, a sequence number) plus IME and
//! focus messages, and the app's "finished" reply that the compositor uses
//! for flow control and responsiveness. When the real `InputChannel`
//! arrives, the same events are written as `InputMessage`s instead; this
//! module and the channel are the parts that get replaced.
//!
//! Layout (little endian), each message:
//!
//! ```text
//! u32 length      total bytes including this header
//! u16 kind        1 motion, 2 key, 3 ime, 4 focus, 0x81 finished
//! u16 version     VERSION
//! u32 seq         sequence number, chosen by the sender
//! ...body
//! ```
//!
//! - motion: i32 device, u32 source, i32 action, i32 action_button,
//!   i32 flags, i32 meta, i32 buttons, i32 classification, i64 down_time,
//!   f32 raw_x, f32 raw_y, u16 pointers, u16 samples, pointers x (i32 id,
//!   i32 tool), samples x (i64 time, pointers x (u8 n, n x (u8 axis,
//!   f32 value)))
//! - key: i32 device, u32 source, i32 action, i32 flags, i32 key_code,
//!   i32 scan_code, i32 meta, i32 repeat, i64 down_time, i64 event_time
//! - ime: u8 op (1 composing, 2 commit, 3 finish), i32 cursor,
//!   i32 target_start, i32 target_end (-1 when none), u32 n, n bytes UTF-8
//! - focus: u8 has_focus
//! - finished: u8 handled, i64 consume_time
//!
//! Surface messages (seq 0) carry the surface handshake between the
//! compositor and the producer of a surface (see
//! [`producer`](crate::producer)):
//!
//! - attach (5, compositor to app): u64 surface, u64 handle (a composition
//!   surface handle already duplicated into the app process, owned by the
//!   app from now on), u32 width, u32 height, i32 z
//! - resize (6, compositor to app): u64 surface, u32 width, u32 height
//! - detach (7, compositor to app): u64 surface
//! - configuration (8, compositor to app): u32 changes, i32 width_px,
//!   i32 height_px (the content area), u32 density_dpi, i32 width_dp,
//!   i32 height_dp, i32 orientation, i64 vsync_period_ns
//! - presented (0x82, app to compositor): u64 surface, u32 width,
//!   u32 height, u8 flags (bit 0: bottom-up content, as OpenGL renders)

use crate::input::{
    ImeEvent, InputEvent, KeyEvent, MotionEvent, MotionSample, PointerCoords, PointerProperties,
};

pub const VERSION: u16 = 1;
pub const HEADER_LEN: usize = 12;
/// Upper bound on pointers and samples per motion event (Android allows
/// 16 pointers).
pub const MAX_POINTERS: usize = 16;
pub const MAX_SAMPLES: usize = 256;

const KIND_MOTION: u16 = 1;
const KIND_KEY: u16 = 2;
const KIND_IME: u16 = 3;
const KIND_FOCUS: u16 = 4;
const KIND_FINISHED: u16 = 0x81;
const KIND_ATTACH: u16 = 5;
const KIND_RESIZE: u16 = 6;
const KIND_DETACH: u16 = 7;
const KIND_CONFIGURATION: u16 = 8;
const KIND_PRESENTED: u16 = 0x82;

/// The window configuration an app process gets with each change (a
/// compact form of [`crate::config::WindowLayout`] until the framework's
/// `Configuration` travels in a `ClientTransaction`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WireConfiguration {
    /// `ActivityInfo.CONFIG_*` bits; 0 for the first configuration.
    pub changes: u32,
    /// The content area (where the app's surfaces go), in app pixels.
    pub width_px: i32,
    pub height_px: i32,
    pub density_dpi: u32,
    pub width_dp: i32,
    pub height_dp: i32,
    /// `Configuration.ORIENTATION_*`.
    pub orientation: i32,
    /// The refresh period of the window's display (the compositor clock).
    pub vsync_period_ns: i64,
}

/// The surface handshake ([`crate::producer`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceMessage {
    /// A new surface for the app. `handle` is a composition surface
    /// handle valid in the app process; the app owns it.
    Attach {
        surface: u64,
        handle: u64,
        width: u32,
        height: u32,
        z: i32,
    },
    /// The size the compositor wants the surface's buffers to have.
    Resize {
        surface: u64,
        width: u32,
        height: u32,
    },
    /// The surface is gone (window closed or surface removed).
    Detach {
        surface: u64,
    },
    Configuration(WireConfiguration),
    /// The producer presented a buffer of this size; `bottom_up` content
    /// (OpenGL row order) is shown flipped. Sent on the first present and
    /// whenever one of the values changes.
    Presented {
        surface: u64,
        width: u32,
        height: u32,
        bottom_up: bool,
    },
}

/// A message on the channel.
#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    Event {
        seq: u32,
        event: InputEvent,
    },
    /// The app finished handling event `seq`.
    Finished {
        seq: u32,
        handled: bool,
        consume_time_ns: i64,
    },
    /// The surface handshake (sequence number 0).
    Surface(SurfaceMessage),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    Truncated,
    BadLength,
    UnknownKind(u16),
    Version(u16),
    TooMany,
    BadUtf8,
    BadValue,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid input message: {self:?}")
    }
}

impl std::error::Error for DecodeError {}

struct W(Vec<u8>);

impl W {
    fn i32(&mut self, v: i32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn i64(&mut self, v: i64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f32(&mut self, v: f32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
}

struct R<'a>(&'a [u8]);

impl R<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        if self.0.len() < N {
            return Err(DecodeError::Truncated);
        }
        let (a, b) = self.0.split_at(N);
        self.0 = b;
        Ok(a.try_into().unwrap())
    }
    fn i32(&mut self) -> Result<i32, DecodeError> {
        Ok(i32::from_le_bytes(self.take()?))
    }
    fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_le_bytes(self.take()?))
    }
    fn u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_le_bytes(self.take()?))
    }
    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take::<1>()?[0])
    }
    fn i64(&mut self) -> Result<i64, DecodeError> {
        Ok(i64::from_le_bytes(self.take()?))
    }
    fn u64(&mut self) -> Result<u64, DecodeError> {
        Ok(u64::from_le_bytes(self.take()?))
    }
    fn f32(&mut self) -> Result<f32, DecodeError> {
        Ok(f32::from_le_bytes(self.take()?))
    }
    fn bytes(&mut self, n: usize) -> Result<&[u8], DecodeError> {
        if self.0.len() < n {
            return Err(DecodeError::Truncated);
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
}

/// Serializes a message.
pub fn encode(msg: &Message) -> Vec<u8> {
    let mut w = W(Vec::with_capacity(64));
    w.u32(0);
    let seq = match msg {
        Message::Event { seq, event } => {
            w.u16(match event {
                InputEvent::Motion(_) => KIND_MOTION,
                InputEvent::Key(_) => KIND_KEY,
                InputEvent::Ime(_) => KIND_IME,
                InputEvent::Focus { .. } => KIND_FOCUS,
            });
            *seq
        }
        Message::Finished { seq, .. } => {
            w.u16(KIND_FINISHED);
            *seq
        }
        Message::Surface(m) => {
            w.u16(match m {
                SurfaceMessage::Attach { .. } => KIND_ATTACH,
                SurfaceMessage::Resize { .. } => KIND_RESIZE,
                SurfaceMessage::Detach { .. } => KIND_DETACH,
                SurfaceMessage::Configuration(_) => KIND_CONFIGURATION,
                SurfaceMessage::Presented { .. } => KIND_PRESENTED,
            });
            0
        }
    };
    w.u16(VERSION);
    w.u32(seq);
    match msg {
        Message::Event { event, .. } => match event {
            InputEvent::Motion(m) => encode_motion(&mut w, m),
            InputEvent::Key(k) => {
                w.i32(k.device_id);
                w.u32(k.source);
                w.i32(k.action);
                w.i32(k.flags);
                w.i32(k.key_code);
                w.i32(k.scan_code);
                w.i32(k.meta_state);
                w.i32(k.repeat_count);
                w.i64(k.down_time_ns);
                w.i64(k.event_time_ns);
            }
            InputEvent::Ime(i) => {
                let (op, cursor, target, text) = match i {
                    ImeEvent::Composing {
                        text,
                        cursor,
                        target,
                    } => (1, *cursor, *target, text.as_str()),
                    ImeEvent::Commit { text } => (2, 0, None, text.as_str()),
                    ImeEvent::FinishComposing => (3, 0, None, ""),
                };
                w.u8(op);
                w.i32(cursor);
                let (s, e) = target.unwrap_or((-1, -1));
                w.i32(s);
                w.i32(e);
                w.u32(text.len() as u32);
                w.0.extend_from_slice(text.as_bytes());
            }
            InputEvent::Focus { has_focus } => w.u8(*has_focus as u8),
        },
        Message::Finished {
            handled,
            consume_time_ns,
            ..
        } => {
            w.u8(*handled as u8);
            w.i64(*consume_time_ns);
        }
        Message::Surface(m) => encode_surface(&mut w, m),
    }
    let len = w.0.len() as u32;
    w.0[..4].copy_from_slice(&len.to_le_bytes());
    w.0
}

fn encode_surface(w: &mut W, m: &SurfaceMessage) {
    match *m {
        SurfaceMessage::Attach {
            surface,
            handle,
            width,
            height,
            z,
        } => {
            w.u64(surface);
            w.u64(handle);
            w.u32(width);
            w.u32(height);
            w.i32(z);
        }
        SurfaceMessage::Resize {
            surface,
            width,
            height,
        } => {
            w.u64(surface);
            w.u32(width);
            w.u32(height);
        }
        SurfaceMessage::Detach { surface } => w.u64(surface),
        SurfaceMessage::Configuration(c) => {
            w.u32(c.changes);
            w.i32(c.width_px);
            w.i32(c.height_px);
            w.u32(c.density_dpi);
            w.i32(c.width_dp);
            w.i32(c.height_dp);
            w.i32(c.orientation);
            w.i64(c.vsync_period_ns);
        }
        SurfaceMessage::Presented {
            surface,
            width,
            height,
            bottom_up,
        } => {
            w.u64(surface);
            w.u32(width);
            w.u32(height);
            w.u8(bottom_up as u8);
        }
    }
}

fn encode_motion(w: &mut W, m: &MotionEvent) {
    let np = m.pointers.len().min(MAX_POINTERS);
    let ns = m.samples.len().min(MAX_SAMPLES);
    w.i32(m.device_id);
    w.u32(m.source);
    w.i32(m.action);
    w.i32(m.action_button);
    w.i32(m.flags);
    w.i32(m.meta_state);
    w.i32(m.button_state);
    w.i32(m.classification);
    w.i64(m.down_time_ns);
    w.f32(m.raw_offset.0);
    w.f32(m.raw_offset.1);
    w.u16(np as u16);
    w.u16(ns as u16);
    for p in &m.pointers[..np] {
        w.i32(p.id);
        w.i32(p.tool_type);
    }
    // The newest samples are kept if there are too many.
    for s in &m.samples[m.samples.len() - ns..] {
        w.i64(s.event_time_ns);
        for i in 0..np {
            let axes = s.coords.get(i).map_or(&[][..], |c| c.axes());
            let axes: Vec<_> = axes.iter().filter(|a| a.0 < 64).collect();
            w.u8(axes.len() as u8);
            for (axis, v) in axes {
                w.u8(*axis as u8);
                w.f32(*v);
            }
        }
    }
}

/// The total length of the message at the start of `buf`, if its header
/// is complete.
pub fn peek_len(buf: &[u8]) -> Option<usize> {
    buf.get(..4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize)
}

/// Parses one message, which must be exactly `buf`.
pub fn decode(buf: &[u8]) -> Result<Message, DecodeError> {
    let mut r = R(buf);
    let len = r.u32()? as usize;
    if len != buf.len() || len < HEADER_LEN {
        return Err(DecodeError::BadLength);
    }
    let kind = r.u16()?;
    let version = r.u16()?;
    if version != VERSION {
        return Err(DecodeError::Version(version));
    }
    let seq = r.u32()?;
    let msg = match kind {
        KIND_MOTION => Message::Event {
            seq,
            event: InputEvent::Motion(decode_motion(&mut r)?),
        },
        KIND_KEY => Message::Event {
            seq,
            event: InputEvent::Key(KeyEvent {
                device_id: r.i32()?,
                source: r.u32()?,
                action: r.i32()?,
                flags: r.i32()?,
                key_code: r.i32()?,
                scan_code: r.i32()?,
                meta_state: r.i32()?,
                repeat_count: r.i32()?,
                down_time_ns: r.i64()?,
                event_time_ns: r.i64()?,
            }),
        },
        KIND_IME => {
            let op = r.u8()?;
            let cursor = r.i32()?;
            let (s, e) = (r.i32()?, r.i32()?);
            let n = r.u32()? as usize;
            let text = std::str::from_utf8(r.bytes(n)?)
                .map_err(|_| DecodeError::BadUtf8)?
                .to_owned();
            let ime = match op {
                1 => ImeEvent::Composing {
                    text,
                    cursor,
                    target: (s >= 0).then_some((s, e)),
                },
                2 => ImeEvent::Commit { text },
                3 => ImeEvent::FinishComposing,
                _ => return Err(DecodeError::BadValue),
            };
            Message::Event {
                seq,
                event: InputEvent::Ime(ime),
            }
        }
        KIND_FOCUS => Message::Event {
            seq,
            event: InputEvent::Focus {
                has_focus: r.u8()? != 0,
            },
        },
        KIND_FINISHED => Message::Finished {
            seq,
            handled: r.u8()? != 0,
            consume_time_ns: r.i64()?,
        },
        KIND_ATTACH => Message::Surface(SurfaceMessage::Attach {
            surface: r.u64()?,
            handle: r.u64()?,
            width: r.u32()?,
            height: r.u32()?,
            z: r.i32()?,
        }),
        KIND_RESIZE => Message::Surface(SurfaceMessage::Resize {
            surface: r.u64()?,
            width: r.u32()?,
            height: r.u32()?,
        }),
        KIND_DETACH => Message::Surface(SurfaceMessage::Detach { surface: r.u64()? }),
        KIND_CONFIGURATION => Message::Surface(SurfaceMessage::Configuration(WireConfiguration {
            changes: r.u32()?,
            width_px: r.i32()?,
            height_px: r.i32()?,
            density_dpi: r.u32()?,
            width_dp: r.i32()?,
            height_dp: r.i32()?,
            orientation: r.i32()?,
            vsync_period_ns: r.i64()?,
        })),
        KIND_PRESENTED => Message::Surface(SurfaceMessage::Presented {
            surface: r.u64()?,
            width: r.u32()?,
            height: r.u32()?,
            bottom_up: match r.u8()? {
                0 => false,
                1 => true,
                _ => return Err(DecodeError::BadValue),
            },
        }),
        k => return Err(DecodeError::UnknownKind(k)),
    };
    if !r.0.is_empty() {
        return Err(DecodeError::BadLength);
    }
    Ok(msg)
}

fn decode_motion(r: &mut R) -> Result<MotionEvent, DecodeError> {
    let device_id = r.i32()?;
    let source = r.u32()?;
    let action = r.i32()?;
    let action_button = r.i32()?;
    let flags = r.i32()?;
    let meta_state = r.i32()?;
    let button_state = r.i32()?;
    let classification = r.i32()?;
    let down_time_ns = r.i64()?;
    let raw_offset = (r.f32()?, r.f32()?);
    let np = r.u16()? as usize;
    let ns = r.u16()? as usize;
    if np == 0 || np > MAX_POINTERS || ns == 0 || ns > MAX_SAMPLES {
        return Err(DecodeError::TooMany);
    }
    let mut pointers = Vec::with_capacity(np);
    for _ in 0..np {
        pointers.push(PointerProperties {
            id: r.i32()?,
            tool_type: r.i32()?,
        });
    }
    let mut samples = Vec::with_capacity(ns);
    for _ in 0..ns {
        let event_time_ns = r.i64()?;
        let mut coords = Vec::with_capacity(np);
        for _ in 0..np {
            let n = r.u8()? as usize;
            let mut axes = Vec::with_capacity(n);
            for _ in 0..n {
                let axis = r.u8()? as u32;
                if axis >= 64 {
                    return Err(DecodeError::BadValue);
                }
                axes.push((axis, r.f32()?));
            }
            coords.push(PointerCoords::from_axes(axes));
        }
        samples.push(MotionSample {
            event_time_ns,
            coords,
        });
    }
    Ok(MotionEvent {
        device_id,
        source,
        action,
        action_button,
        flags,
        meta_state,
        button_state,
        classification,
        down_time_ns,
        raw_offset,
        pointers,
        samples,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::android::motion::*;
    use crate::input::keys::system_back;

    fn motion() -> MotionEvent {
        MotionEvent {
            device_id: 3,
            source: 0x1002,
            action: with_index(ACTION_POINTER_DOWN, 1),
            action_button: 0,
            flags: 0,
            meta_state: 0x1000,
            button_state: 0,
            classification: 0,
            down_time_ns: 123_456_789,
            raw_offset: (100.0, 50.0),
            pointers: vec![
                PointerProperties {
                    id: 0,
                    tool_type: TOOL_TYPE_FINGER,
                },
                PointerProperties {
                    id: 1,
                    tool_type: TOOL_TYPE_FINGER,
                },
            ],
            samples: (0..3)
                .map(|i| MotionSample {
                    event_time_ns: 123_456_789 + i * 4_000_000,
                    coords: vec![
                        PointerCoords::xy(1.5, 2.5).with(AXIS_PRESSURE, 0.5),
                        PointerCoords::xy(10.0, 20.0).with(AXIS_TOUCH_MAJOR, 8.0),
                    ],
                })
                .collect(),
        }
    }

    #[test]
    fn round_trips() {
        let [back, _] = system_back(77);
        let msgs = vec![
            Message::Event {
                seq: 1,
                event: InputEvent::Motion(motion()),
            },
            Message::Event {
                seq: 2,
                event: InputEvent::Key(back),
            },
            Message::Event {
                seq: 3,
                event: InputEvent::Ime(ImeEvent::Composing {
                    text: "שלום".into(),
                    cursor: 4,
                    target: Some((0, 2)),
                }),
            },
            Message::Event {
                seq: 4,
                event: InputEvent::Ime(ImeEvent::Commit {
                    text: "日本語".into(),
                }),
            },
            Message::Event {
                seq: 5,
                event: InputEvent::Ime(ImeEvent::FinishComposing),
            },
            Message::Event {
                seq: 6,
                event: InputEvent::Focus { has_focus: true },
            },
            Message::Finished {
                seq: 1,
                handled: true,
                consume_time_ns: 99,
            },
            Message::Surface(SurfaceMessage::Attach {
                surface: 7,
                handle: 0x1234,
                width: 640,
                height: 480,
                z: -1,
            }),
            Message::Surface(SurfaceMessage::Resize {
                surface: 7,
                width: 400,
                height: 600,
            }),
            Message::Surface(SurfaceMessage::Detach { surface: 7 }),
            Message::Surface(SurfaceMessage::Configuration(WireConfiguration {
                changes: 0x480,
                width_px: 400,
                height_px: 600,
                density_dpi: 240,
                width_dp: 266,
                height_dp: 400,
                orientation: 1,
                vsync_period_ns: 8_333_333,
            })),
            Message::Surface(SurfaceMessage::Presented {
                surface: 7,
                width: 400,
                height: 600,
                bottom_up: true,
            }),
        ];
        for m in msgs {
            let b = encode(&m);
            assert_eq!(peek_len(&b), Some(b.len()));
            assert_eq!(decode(&b).unwrap(), m);
        }
    }

    #[test]
    fn motion_size_is_compact() {
        // 2 pointers, 3 samples: header 12 + fixed 52 + pointers 16 +
        // samples 3 * (8 + 2 * (1 + 3 * 5)) = 168.
        let b = encode(&Message::Event {
            seq: 1,
            event: InputEvent::Motion(motion()),
        });
        assert_eq!(b.len(), 12 + 52 + 16 + 3 * (8 + 2 * 16));
    }

    #[test]
    fn rejects_malformed_input() {
        let good = encode(&Message::Event {
            seq: 1,
            event: InputEvent::Motion(motion()),
        });
        // Every truncation is rejected, never a panic.
        for n in 0..good.len() {
            let mut b = good[..n].to_vec();
            if n >= 4 {
                b[..4].copy_from_slice(&(n as u32).to_le_bytes());
            }
            assert!(decode(&b).is_err(), "{n}");
        }
        let mut v = good.clone();
        v[6] = 9;
        assert_eq!(decode(&v), Err(DecodeError::Version(9)));
        let mut k = good.clone();
        k[4] = 0x55;
        assert_eq!(decode(&k), Err(DecodeError::UnknownKind(0x55)));
        let mut extra = good.clone();
        extra.push(0);
        assert_eq!(decode(&extra), Err(DecodeError::BadLength));
        // Bit flips anywhere: no panic.
        for i in 0..good.len() {
            let mut f = good.clone();
            f[i] ^= 0xA5;
            let _ = decode(&f);
        }
        let ime = encode(&Message::Event {
            seq: 1,
            event: InputEvent::Ime(ImeEvent::Commit { text: "a".into() }),
        });
        let mut bad = ime.clone();
        *bad.last_mut().unwrap() = 0xFF;
        assert_eq!(decode(&bad), Err(DecodeError::BadUtf8));
    }
}
