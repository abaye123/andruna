use super::*;
use crate::testkit::apk::{ApkSpec, IconSpec};
use crate::testkit::zipw::ZipWriter;

fn render_spec(icon: IconSpec) -> Result<Icon, String> {
    let mut spec = ApkSpec::new("com.example.icon");
    spec.icon = icon;
    let mut w = ZipWriter::new();
    for e in spec.entries() {
        w.add(e);
    }
    let bytes = w.finish();
    let apk = Archive::parse(&bytes).unwrap();
    let table = ResTable::parse(&apk.read_name("resources.arsc").unwrap().unwrap()).unwrap();
    let m =
        crate::manifest::parse(&apk.read_name("AndroidManifest.xml").unwrap().unwrap()).unwrap();
    render(&apk, Some(&table), m.icon.unwrap())
}

fn pixel(png: &[u8], x: u32, y: u32) -> (u8, u8, u8, u8) {
    let p = Pixmap::decode_png(png).unwrap();
    let c = p.pixel(x, y).unwrap().demultiply();
    (c.red(), c.green(), c.blue(), c.alpha())
}

fn ico_entries(ico: &[u8]) -> Vec<(u32, Vec<u8>)> {
    let n = u16::from_le_bytes([ico[4], ico[5]]) as usize;
    (0..n)
        .map(|i| {
            let e = &ico[6 + 16 * i..6 + 16 * (i + 1)];
            let size = if e[0] == 0 { 256 } else { e[0] as u32 };
            let len = u32::from_le_bytes(e[8..12].try_into().unwrap()) as usize;
            let off = u32::from_le_bytes(e[12..16].try_into().unwrap()) as usize;
            (size, ico[off..off + len].to_vec())
        })
        .collect()
}

#[test]
fn adaptive_icon_layers_and_mask() {
    let icon = render_spec(IconSpec::Adaptive {
        background: 0xFF1E88E5,
        foreground: 0xFFFFFFFF,
    })
    .unwrap();
    assert_eq!(icon.kind, "adaptive-icon");
    // Center: the white foreground circle.
    assert_eq!(pixel(&icon.png256, 128, 128), (255, 255, 255, 255));
    // Inside the mask but outside the circle: the background color. The
    // circle (radius 20 of 72 visible dp) covers 71 px around the center.
    assert_eq!(pixel(&icon.png256, 128, 30), (0x1E, 0x88, 0xE5, 255));
    // The corner is outside the rounded mask.
    assert_eq!(pixel(&icon.png256, 1, 1).3, 0);
    // The .ico has every size, each a PNG of that size.
    let entries = ico_entries(&icon.ico);
    assert_eq!(entries.iter().map(|e| e.0).collect::<Vec<_>>(), ICO_SIZES);
    for (size, png) in entries {
        let p = Pixmap::decode_png(&png).unwrap();
        assert_eq!((p.width(), p.height()), (size, size));
    }
}

#[test]
fn legacy_png_icon() {
    let icon = render_spec(IconSpec::Png(0xFFFF0000)).unwrap();
    assert_eq!(icon.kind, "bitmap");
    assert_eq!(pixel(&icon.png256, 128, 128), (255, 0, 0, 255));
    assert_eq!(pixel(&icon.png256, 2, 2).3, 0);
    let small = ico_entries(&icon.ico).remove(0).1;
    let (r, _, _, a) = pixel(&small, 8, 8);
    assert!(r > 200 && a > 200, "downscaled center stays red");
}

#[test]
fn missing_icon_is_an_error_not_a_panic() {
    let mut spec = ApkSpec::new("com.example.icon");
    spec.icon = IconSpec::None;
    let mut w = ZipWriter::new();
    for e in spec.entries() {
        w.add(e);
    }
    let bytes = w.finish();
    let apk = Archive::parse(&bytes).unwrap();
    assert!(render(&apk, None, 0x7f02_0000).is_err());
}

#[test]
fn ico_layout() {
    let ico = encode_ico(&[(16, vec![1, 2, 3]), (256, vec![4])]);
    assert_eq!(&ico[..6], &[0, 0, 1, 0, 2, 0]);
    let e = ico_entries(&ico);
    assert_eq!(e, vec![(16, vec![1, 2, 3]), (256, vec![4])]);
}
