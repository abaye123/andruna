//! A builder for small test APKs: manifest, resources (localized label,
//! PNG or adaptive icon), activities, permissions, native libraries, code,
//! and split APKs.

use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Transform};

use super::arscw::{TableBuilder, Val};
use super::axmlw::{XmlAttr, XmlElement, XmlValue, write_xml};
use super::sign::{SignConfig, sign};
use super::zipw::ZipEntry;
use crate::manifest::{ACTION_MAIN, CATEGORY_LAUNCHER, attr};

/// The launcher icon of a test APK.
#[derive(Clone, Debug)]
pub enum IconSpec {
    None,
    /// A legacy PNG icon: a filled circle of this ARGB color.
    Png(u32),
    /// An adaptive icon: a background color and a vector foreground (a
    /// circle of radius 20 at the center of the 108 viewport), with a PNG
    /// fallback for older platforms.
    Adaptive {
        background: u32,
        foreground: u32,
    },
}

#[derive(Clone, Debug)]
pub struct ActivitySpec {
    /// Class name as written in the manifest (`.Main` or fully qualified).
    pub name: String,
    pub launcher: bool,
    pub enabled: bool,
}

impl ActivitySpec {
    pub fn launcher(name: &str) -> Self {
        Self {
            name: name.into(),
            launcher: true,
            enabled: true,
        }
    }
    pub fn plain(name: &str) -> Self {
        Self {
            name: name.into(),
            launcher: false,
            enabled: true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ApkSpec {
    pub package: String,
    pub version_code: u32,
    pub version_name: String,
    pub min_sdk: u32,
    pub target_sdk: u32,
    /// Label per qualifier (`""` default, `iw`, `fr-rCA`, ...).
    pub labels: Vec<(String, String)>,
    pub icon: IconSpec,
    pub activities: Vec<ActivitySpec>,
    pub permissions: Vec<String>,
    /// `lib/<abi>/libtest.so` for each ABI.
    pub native_abis: Vec<String>,
    pub extract_native_libs: bool,
    /// `classes.dex`; without it the manifest says `hasCode="false"`.
    pub dex: Option<Vec<u8>>,
    /// Set for a split APK (`config.x86_64`, `config.he`, ...).
    pub split: Option<String>,
    pub extra: Vec<ZipEntry>,
    /// Store `resources.arsc` compressed (an alignment error from API 30).
    pub compress_resources: bool,
}

impl ApkSpec {
    pub fn new(package: &str) -> Self {
        Self {
            package: package.into(),
            version_code: 1,
            version_name: "1.0".into(),
            min_sdk: 24,
            target_sdk: 34,
            labels: vec![
                ("".into(), "Test App".into()),
                ("iw".into(), "\u{05d9}\u{05d9}\u{05e9}\u{05d5}\u{05dd} \u{05d1}\u{05d3}\u{05d9}\u{05e7}\u{05d4}".into()),
            ],
            icon: IconSpec::Adaptive {
                background: 0xFF1E88E5,
                foreground: 0xFFFFFFFF,
            },
            activities: vec![ActivitySpec::launcher(".MainActivity")],
            permissions: vec!["android.permission.INTERNET".into()],
            native_abis: Vec::new(),
            extract_native_libs: true,
            dex: None,
            split: None,
            extra: Vec::new(),
            compress_resources: false,
        }
    }

    /// A config split of this package.
    pub fn split_of(base: &ApkSpec, split: &str) -> Self {
        let mut s = Self::new(&base.package);
        s.version_code = base.version_code;
        s.version_name = base.version_name.clone();
        s.split = Some(split.into());
        s.labels = Vec::new();
        s.icon = IconSpec::None;
        s.activities = Vec::new();
        s.permissions = Vec::new();
        s
    }

    fn manifest(&self, label: Option<u32>, icon: Option<u32>) -> Vec<u8> {
        let mut m = XmlElement::new("manifest")
            .attr(XmlAttr::plain_str("package", &self.package))
            .attr(XmlAttr::android_int(
                "versionCode",
                attr::VERSION_CODE,
                self.version_code,
            ))
            .attr(XmlAttr::android_str(
                "versionName",
                attr::VERSION_NAME,
                &self.version_name,
            ));
        if let Some(s) = &self.split {
            m = m.attr(XmlAttr::plain_str("split", s));
        }
        m = m.child(
            XmlElement::new("uses-sdk")
                .attr(XmlAttr::android_int(
                    "minSdkVersion",
                    attr::MIN_SDK_VERSION,
                    self.min_sdk,
                ))
                .attr(XmlAttr::android_int(
                    "targetSdkVersion",
                    attr::TARGET_SDK_VERSION,
                    self.target_sdk,
                )),
        );
        for p in &self.permissions {
            m = m.child(
                XmlElement::new("uses-permission").attr(XmlAttr::android_str(
                    "name",
                    attr::NAME,
                    p,
                )),
            );
        }
        let mut app = XmlElement::new("application")
            .attr(XmlAttr::android_bool(
                "hasCode",
                attr::HAS_CODE,
                self.dex.is_some(),
            ))
            .attr(XmlAttr::android_bool(
                "extractNativeLibs",
                attr::EXTRACT_NATIVE_LIBS,
                self.extract_native_libs,
            ));
        if let Some(l) = label {
            app = app.attr(XmlAttr::android_ref("label", attr::LABEL, l));
        }
        if let Some(i) = icon {
            app = app.attr(XmlAttr::android_ref("icon", attr::ICON, i));
        }
        for a in &self.activities {
            let mut el = XmlElement::new("activity")
                .attr(XmlAttr::android_str("name", attr::NAME, &a.name))
                .attr(XmlAttr::android_bool(
                    "exported",
                    attr::EXPORTED,
                    a.launcher,
                ))
                .attr(XmlAttr::android(
                    "enabled",
                    attr::ENABLED,
                    XmlValue::Bool(a.enabled),
                ));
            if a.launcher {
                el = el.child(
                    XmlElement::new("intent-filter")
                        .child(XmlElement::new("action").attr(XmlAttr::android_str(
                            "name",
                            attr::NAME,
                            ACTION_MAIN,
                        )))
                        .child(XmlElement::new("category").attr(XmlAttr::android_str(
                            "name",
                            attr::NAME,
                            CATEGORY_LAUNCHER,
                        ))),
                );
            }
            app = app.child(el);
        }
        write_xml(&m.child(app), false)
    }

    /// The archive entries, unsigned.
    pub fn entries(&self) -> Vec<ZipEntry> {
        let mut t = TableBuilder::new(&self.package);
        let mut files = Vec::new();
        let label = (!self.labels.is_empty()).then(|| {
            let vals: Vec<(&str, Val)> = self
                .labels
                .iter()
                .map(|(q, l)| (q.as_str(), Val::Str(l.clone())))
                .collect();
            t.add("string", "app_name", &vals)
        });
        let icon = match &self.icon {
            IconSpec::None => None,
            IconSpec::Png(c) => {
                files.push(ZipEntry::stored(
                    "res/mipmap-xxxhdpi/ic_launcher.png",
                    circle_png(192, *c),
                ));
                Some(t.add(
                    "mipmap",
                    "ic_launcher",
                    &[(
                        "xxxhdpi",
                        Val::Str("res/mipmap-xxxhdpi/ic_launcher.png".into()),
                    )],
                ))
            }
            IconSpec::Adaptive {
                background,
                foreground,
            } => {
                let bg = t.add("color", "ic_bg", &[("", Val::Color(*background))]);
                let fg_color = t.add("color", "ic_fg", &[("", Val::Color(*foreground))]);
                let fg = t.add(
                    "drawable",
                    "ic_fg",
                    &[("", Val::Str("res/drawable/ic_fg.xml".into()))],
                );
                let vector = XmlElement::new("vector")
                    .attr(XmlAttr::android(
                        "width",
                        0x0101_0159,
                        XmlValue::Dimension((108 << 8) | 1),
                    ))
                    .attr(XmlAttr::android(
                        "height",
                        0x0101_0155,
                        XmlValue::Dimension((108 << 8) | 1),
                    ))
                    .attr(XmlAttr::android(
                        "viewportWidth",
                        0x0101_0402,
                        XmlValue::Float(108.0),
                    ))
                    .attr(XmlAttr::android(
                        "viewportHeight",
                        0x0101_0403,
                        XmlValue::Float(108.0),
                    ))
                    .child(
                        XmlElement::new("path")
                            .attr(XmlAttr::android(
                                "fillColor",
                                0x0101_0404,
                                XmlValue::Ref(fg_color),
                            ))
                            .attr(XmlAttr::android_str(
                                "pathData",
                                0x0101_0405,
                                "M54,34a20,20 0 1,1 0,40a20,20 0 1,1 0,-40z",
                            )),
                    );
                files.push(ZipEntry::deflated(
                    "res/drawable/ic_fg.xml",
                    write_xml(&vector, false),
                ));
                let adaptive = XmlElement::new("adaptive-icon")
                    .child(XmlElement::new("background").attr(XmlAttr::android_ref(
                        "drawable",
                        0x0101_0199,
                        bg,
                    )))
                    .child(XmlElement::new("foreground").attr(XmlAttr::android_ref(
                        "drawable",
                        0x0101_0199,
                        fg,
                    )));
                files.push(ZipEntry::deflated(
                    "res/mipmap-anydpi-v26/ic_launcher.xml",
                    write_xml(&adaptive, false),
                ));
                files.push(ZipEntry::stored(
                    "res/mipmap-xxxhdpi/ic_launcher.png",
                    circle_png(192, *background),
                ));
                Some(t.add(
                    "mipmap",
                    "ic_launcher",
                    &[
                        (
                            "xxxhdpi",
                            Val::Str("res/mipmap-xxxhdpi/ic_launcher.png".into()),
                        ),
                        (
                            "anydpi-v26",
                            Val::Str("res/mipmap-anydpi-v26/ic_launcher.xml".into()),
                        ),
                    ],
                ))
            }
        };
        let mut out = vec![ZipEntry::deflated(
            "AndroidManifest.xml",
            self.manifest(label, icon),
        )];
        if let Some(dex) = &self.dex {
            out.push(ZipEntry::deflated("classes.dex", dex.clone()));
        }
        if label.is_some() || icon.is_some() {
            let arsc = ZipEntry::stored("resources.arsc", t.build()).align(4);
            out.push(if self.compress_resources {
                ZipEntry::deflated("resources.arsc", arsc.data)
            } else {
                arsc
            });
        }
        out.extend(files);
        for abi in &self.native_abis {
            let so = fake_elf(abi);
            let name = format!("lib/{abi}/libtest.so");
            out.push(if self.extract_native_libs {
                ZipEntry::deflated(&name, so)
            } else {
                ZipEntry::stored(&name, so).align(4096)
            });
        }
        out.extend(self.extra.iter().cloned());
        out
    }

    /// The signed APK.
    pub fn build(&self, cfg: &SignConfig) -> Vec<u8> {
        sign(&self.entries(), cfg)
    }
}

/// A PNG of a filled circle.
pub fn circle_png(size: u32, argb: u32) -> Vec<u8> {
    let mut pm = Pixmap::new(size, size).unwrap();
    let mut paint = Paint::default();
    paint.set_color_rgba8(
        (argb >> 16) as u8,
        (argb >> 8) as u8,
        argb as u8,
        (argb >> 24) as u8,
    );
    paint.anti_alias = true;
    let r = size as f32 / 2.0;
    let path = PathBuilder::from_circle(r, r, r * 0.9).unwrap();
    pm.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );
    pm.encode_png().unwrap()
}

/// Bytes that start like an ELF shared object for `abi` (the installer
/// copies native libraries; it does not load them).
pub fn fake_elf(abi: &str) -> Vec<u8> {
    let machine: u16 = match abi {
        "x86_64" => 62,
        "arm64-v8a" => 183,
        "x86" => 3,
        _ => 40,
    };
    let class = if abi.contains("64") { 2u8 } else { 1 };
    let mut v = vec![0x7f, b'E', b'L', b'F', class, 1, 1, 0];
    v.extend_from_slice(&[0u8; 8]);
    v.extend_from_slice(&3u16.to_le_bytes());
    v.extend_from_slice(&machine.to_le_bytes());
    v.extend(std::iter::repeat_n(0u8, 512));
    v
}
