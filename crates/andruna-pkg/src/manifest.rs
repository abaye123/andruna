//! The manifest model: what an installer reads from `AndroidManifest.xml`.
//!
//! Framework attributes are matched by resource ID (as Android's parser
//! does), with the attribute name in the Android namespace as a fallback.

use serde::{Deserialize, Serialize};

use crate::axml::{self, Element, Value};
use crate::error::{Code, Error, Result};

/// `android.R.attr` IDs used here.
pub mod attr {
    pub const LABEL: u32 = 0x0101_0001;
    pub const ICON: u32 = 0x0101_0002;
    pub const NAME: u32 = 0x0101_0003;
    pub const HAS_CODE: u32 = 0x0101_000c;
    pub const ENABLED: u32 = 0x0101_000e;
    pub const EXPORTED: u32 = 0x0101_0010;
    pub const TARGET_ACTIVITY: u32 = 0x0101_0202;
    pub const MIN_SDK_VERSION: u32 = 0x0101_020c;
    pub const VERSION_CODE: u32 = 0x0101_021b;
    pub const VERSION_NAME: u32 = 0x0101_021c;
    pub const TARGET_SDK_VERSION: u32 = 0x0101_0270;
    pub const MAX_SDK_VERSION: u32 = 0x0101_0271;
    pub const EXTRACT_NATIVE_LIBS: u32 = 0x0101_04ea;
    pub const ROUND_ICON: u32 = 0x0101_052c;
    pub const VERSION_CODE_MAJOR: u32 = 0x0101_0576;
}

pub const ACTION_MAIN: &str = "android.intent.action.MAIN";
pub const CATEGORY_LAUNCHER: &str = "android.intent.category.LAUNCHER";

/// A string attribute that may be a literal or a resource reference.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResString {
    Literal(String),
    Resource(u32),
}

/// An intent filter.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentFilter {
    pub actions: Vec<String>,
    pub categories: Vec<String>,
    /// `scheme://host` pairs and MIME types, kept for later file and link
    /// association (D-17).
    pub schemes: Vec<String>,
    pub mime_types: Vec<String>,
}

impl IntentFilter {
    pub fn is_launcher(&self) -> bool {
        self.actions.iter().any(|a| a == ACTION_MAIN)
            && self.categories.iter().any(|c| c == CATEGORY_LAUNCHER)
    }
}

/// An activity or activity alias.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Activity {
    /// Fully qualified class name (for an alias, the alias name).
    pub name: String,
    /// For an alias, the fully qualified target activity.
    pub target: Option<String>,
    pub enabled: bool,
    pub exported: Option<bool>,
    pub label: Option<ResString>,
    pub icon: Option<u32>,
    pub filters: Vec<IntentFilter>,
}

impl Activity {
    pub fn is_launcher(&self) -> bool {
        self.enabled && self.filters.iter().any(IntentFilter::is_launcher)
    }
}

/// A requested permission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Permission {
    pub name: String,
    pub max_sdk: Option<u32>,
}

/// The parts of the manifest an installer uses.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub package: String,
    /// `versionCodeMajor << 32 | versionCode`.
    pub version_code: u64,
    pub version_name: Option<String>,
    /// The split name for a split APK; `None` for a base APK.
    pub split: Option<String>,
    pub min_sdk: u32,
    pub target_sdk: u32,
    pub max_sdk: Option<u32>,
    pub label: Option<ResString>,
    pub icon: Option<u32>,
    pub round_icon: Option<u32>,
    pub has_code: bool,
    /// `android:extractNativeLibs` (default true).
    pub extract_native_libs: bool,
    pub activities: Vec<Activity>,
    pub permissions: Vec<Permission>,
}

impl Manifest {
    /// The activities (and aliases) a launcher shows, in manifest order.
    pub fn launcher_activities(&self) -> impl Iterator<Item = &Activity> {
        self.activities.iter().filter(|a| a.is_launcher())
    }
}

fn bad(m: impl Into<String>) -> Error {
    Error::new(Code::InvalidManifest, m)
}

/// Resolves a class name the way the package parser does.
pub fn class_name(package: &str, name: &str) -> String {
    if let Some(rest) = name.strip_prefix('.') {
        format!("{package}.{rest}")
    } else if !name.contains('.') {
        format!("{package}.{name}")
    } else {
        name.to_string()
    }
}

fn int_attr(e: &Element, id: u32, name: &str) -> Option<u32> {
    let a = e.android_attr(id, name)?;
    match &a.value {
        Value::String(s) => s.trim().parse().ok(),
        v => v.as_int(),
    }
}

fn bool_attr(e: &Element, id: u32, name: &str) -> Option<bool> {
    let a = e.android_attr(id, name)?;
    match &a.value {
        Value::Bool(b) => Some(*b),
        Value::Int(v) => Some(*v != 0),
        Value::String(s) => match s.as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn str_attr(e: &Element, id: u32, name: &str) -> Option<String> {
    e.android_attr(id, name)
        .and_then(|a| a.value.as_str().map(str::to_string))
}

fn res_string(e: &Element, id: u32, name: &str) -> Option<ResString> {
    match &e.android_attr(id, name)?.value {
        Value::String(s) => Some(ResString::Literal(s.clone())),
        Value::Reference(r) => Some(ResString::Resource(*r)),
        _ => None,
    }
}

fn ref_attr(e: &Element, id: u32, name: &str) -> Option<u32> {
    match e.android_attr(id, name)?.value {
        Value::Reference(r) => Some(r),
        _ => None,
    }
}

/// SDK attributes may be a number or a preview codename. A codename means
/// a platform newer than any released one.
fn sdk_attr(e: &Element, id: u32, name: &str) -> Option<u32> {
    let a = e.android_attr(id, name)?;
    match &a.value {
        Value::String(s) => Some(s.trim().parse().unwrap_or(10_000)),
        v => v.as_int(),
    }
}

fn filter(f: &Element) -> IntentFilter {
    let mut out = IntentFilter::default();
    for c in &f.children {
        let name = str_attr(c, attr::NAME, "name");
        match (c.name.as_str(), name) {
            ("action", Some(n)) => out.actions.push(n),
            ("category", Some(n)) => out.categories.push(n),
            ("data", _) => {
                if let Some(s) = c
                    .attrs
                    .iter()
                    .find(|a| a.name == "scheme")
                    .and_then(|a| a.value.as_str())
                {
                    out.schemes.push(s.to_string());
                }
                if let Some(m) = c
                    .attrs
                    .iter()
                    .find(|a| a.name == "mimeType")
                    .and_then(|a| a.value.as_str())
                {
                    out.mime_types.push(m.to_string());
                }
            }
            _ => {}
        }
    }
    out
}

/// Reads the manifest model from a parsed document.
pub fn from_element(root: &Element) -> Result<Manifest> {
    if root.name != "manifest" {
        return Err(bad(format!(
            "root element is <{}>, not <manifest>",
            root.name
        )));
    }
    let package = root
        .plain_attr("package")
        .and_then(|a| a.value.as_str())
        .ok_or_else(|| bad("<manifest> has no package attribute"))?
        .to_string();
    if !is_valid_package_name(&package) {
        return Err(bad(format!("`{package}` is not a valid package name")));
    }
    let code = int_attr(root, attr::VERSION_CODE, "versionCode").unwrap_or(0) as u64;
    let major = int_attr(root, attr::VERSION_CODE_MAJOR, "versionCodeMajor").unwrap_or(0) as u64;
    let split = root
        .plain_attr("split")
        .and_then(|a| a.value.as_str())
        .map(str::to_string);
    let mut m = Manifest {
        package: package.clone(),
        version_code: (major << 32) | code,
        version_name: str_attr(root, attr::VERSION_NAME, "versionName"),
        split,
        min_sdk: 1,
        target_sdk: 0,
        max_sdk: None,
        label: None,
        icon: None,
        round_icon: None,
        has_code: true,
        extract_native_libs: true,
        activities: Vec::new(),
        permissions: Vec::new(),
    };
    let mut target = None;
    for c in &root.children {
        match c.name.as_str() {
            "uses-sdk" => {
                if let Some(v) = sdk_attr(c, attr::MIN_SDK_VERSION, "minSdkVersion") {
                    m.min_sdk = v;
                }
                target = sdk_attr(c, attr::TARGET_SDK_VERSION, "targetSdkVersion").or(target);
                m.max_sdk = sdk_attr(c, attr::MAX_SDK_VERSION, "maxSdkVersion");
            }
            "uses-permission" | "uses-permission-sdk-23" | "uses-permission-sdk-m" => {
                if let Some(name) = str_attr(c, attr::NAME, "name") {
                    let max_sdk = int_attr(c, attr::MAX_SDK_VERSION, "maxSdkVersion");
                    if !m.permissions.iter().any(|p| p.name == name) {
                        m.permissions.push(Permission { name, max_sdk });
                    }
                }
            }
            "application" => {
                m.label = res_string(c, attr::LABEL, "label");
                m.icon = ref_attr(c, attr::ICON, "icon");
                m.round_icon = ref_attr(c, attr::ROUND_ICON, "roundIcon");
                m.has_code = bool_attr(c, attr::HAS_CODE, "hasCode").unwrap_or(true);
                m.extract_native_libs =
                    bool_attr(c, attr::EXTRACT_NATIVE_LIBS, "extractNativeLibs").unwrap_or(true);
                for a in &c.children {
                    let alias = a.name == "activity-alias";
                    if a.name != "activity" && !alias {
                        continue;
                    }
                    let Some(name) = str_attr(a, attr::NAME, "name") else {
                        return Err(bad(format!("<{}> without android:name", a.name)));
                    };
                    let target = if alias {
                        match str_attr(a, attr::TARGET_ACTIVITY, "targetActivity") {
                            Some(t) => Some(class_name(&package, &t)),
                            None => return Err(bad("<activity-alias> without targetActivity")),
                        }
                    } else {
                        None
                    };
                    m.activities.push(Activity {
                        name: class_name(&package, &name),
                        target,
                        enabled: bool_attr(a, attr::ENABLED, "enabled").unwrap_or(true),
                        exported: bool_attr(a, attr::EXPORTED, "exported"),
                        label: res_string(a, attr::LABEL, "label"),
                        icon: ref_attr(a, attr::ICON, "icon"),
                        filters: a.children_named("intent-filter").map(filter).collect(),
                    });
                }
            }
            _ => {}
        }
    }
    // targetSdkVersion defaults to minSdkVersion.
    m.target_sdk = target.unwrap_or(m.min_sdk);
    Ok(m)
}

/// Parses `AndroidManifest.xml` bytes.
pub fn parse(data: &[u8]) -> Result<Manifest> {
    from_element(&axml::parse(data)?)
}

/// The package name rule of Android's parser (shared with the pipe
/// protocol's check).
pub fn is_valid_package_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 255 {
        return false;
    }
    let mut segments = 0;
    for seg in name.split('.') {
        let mut chars = seg.chars();
        match chars.next() {
            Some(c) if c.is_ascii_alphabetic() => {}
            _ => return false,
        }
        if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return false;
        }
        segments += 1;
    }
    segments >= 2
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::axmlw::{XmlAttr, XmlElement, XmlValue, write_xml};

    fn intent(action: &str, cat: &str) -> XmlElement {
        XmlElement::new("intent-filter")
            .child(XmlElement::new("action").attr(XmlAttr::android_str("name", attr::NAME, action)))
            .child(XmlElement::new("category").attr(XmlAttr::android_str("name", attr::NAME, cat)))
    }

    #[test]
    fn reads_a_full_manifest() {
        let doc = XmlElement::new("manifest")
            .attr(XmlAttr::plain_str("package", "org.example.notes"))
            .attr(XmlAttr::android_int("versionCode", attr::VERSION_CODE, 7))
            .attr(XmlAttr::android_str(
                "versionName",
                attr::VERSION_NAME,
                "1.2",
            ))
            .child(
                XmlElement::new("uses-sdk")
                    .attr(XmlAttr::android_int(
                        "minSdkVersion",
                        attr::MIN_SDK_VERSION,
                        21,
                    ))
                    .attr(XmlAttr::android_str(
                        "targetSdkVersion",
                        attr::TARGET_SDK_VERSION,
                        "34",
                    )),
            )
            .child(
                XmlElement::new("uses-permission").attr(XmlAttr::android_str(
                    "name",
                    attr::NAME,
                    "android.permission.INTERNET",
                )),
            )
            .child(
                XmlElement::new("application")
                    .attr(XmlAttr::android_ref("label", attr::LABEL, 0x7f01_0000))
                    .attr(XmlAttr::android_ref("icon", attr::ICON, 0x7f02_0000))
                    .attr(XmlAttr::android_bool(
                        "extractNativeLibs",
                        attr::EXTRACT_NATIVE_LIBS,
                        false,
                    ))
                    .child(
                        XmlElement::new("activity")
                            .attr(XmlAttr::android_str("name", attr::NAME, ".Main"))
                            .child(intent(ACTION_MAIN, CATEGORY_LAUNCHER)),
                    )
                    .child(XmlElement::new("activity").attr(XmlAttr::android_str(
                        "name",
                        attr::NAME,
                        "Settings",
                    )))
                    .child(
                        XmlElement::new("activity-alias")
                            .attr(XmlAttr::android_str("name", attr::NAME, "org.other.Alias"))
                            .attr(XmlAttr::android_str(
                                "targetActivity",
                                attr::TARGET_ACTIVITY,
                                ".Main",
                            ))
                            .attr(XmlAttr::android(
                                "enabled",
                                attr::ENABLED,
                                XmlValue::Bool(false),
                            ))
                            .child(intent(ACTION_MAIN, CATEGORY_LAUNCHER)),
                    ),
            );
        let m = parse(&write_xml(&doc, false)).unwrap();
        assert_eq!(m.package, "org.example.notes");
        assert_eq!(m.version_code, 7);
        assert_eq!(m.version_name.as_deref(), Some("1.2"));
        assert_eq!((m.min_sdk, m.target_sdk), (21, 34));
        assert_eq!(m.label, Some(ResString::Resource(0x7f01_0000)));
        assert_eq!(m.icon, Some(0x7f02_0000));
        assert!(!m.extract_native_libs);
        assert_eq!(m.permissions[0].name, "android.permission.INTERNET");
        let names: Vec<_> = m.activities.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "org.example.notes.Main",
                "org.example.notes.Settings",
                "org.other.Alias"
            ]
        );
        assert_eq!(
            m.activities[2].target.as_deref(),
            Some("org.example.notes.Main")
        );
        let launchers: Vec<_> = m.launcher_activities().map(|a| a.name.as_str()).collect();
        assert_eq!(
            launchers,
            ["org.example.notes.Main"],
            "disabled alias excluded"
        );
    }

    #[test]
    fn rejects_bad_manifests() {
        let no_pkg = XmlElement::new("manifest");
        assert_eq!(
            parse(&write_xml(&no_pkg, false)).unwrap_err().code,
            Code::InvalidManifest
        );
        let bad_pkg = XmlElement::new("manifest").attr(XmlAttr::plain_str("package", "nodots"));
        assert!(parse(&write_xml(&bad_pkg, false)).is_err());
        let wrong_root = XmlElement::new("application");
        assert!(parse(&write_xml(&wrong_root, false)).is_err());
        let codename = XmlElement::new("manifest")
            .attr(XmlAttr::plain_str("package", "a.b"))
            .child(XmlElement::new("uses-sdk").attr(XmlAttr::android_str(
                "minSdkVersion",
                attr::MIN_SDK_VERSION,
                "Baklava",
            )));
        assert!(parse(&write_xml(&codename, false)).unwrap().min_sdk > crate::PLATFORM_SDK);
    }

    #[test]
    fn class_names() {
        assert_eq!(class_name("a.b", ".X"), "a.b.X");
        assert_eq!(class_name("a.b", "X"), "a.b.X");
        assert_eq!(class_name("a.b", "c.d.X"), "c.d.X");
    }
}
