//! Reading an APK set: each APK parsed and verified, the set checked to be
//! one package, splits and ABI selected.
//!
//! Rules applied, as Android's installer applies them:
//!
//! - `AndroidManifest.xml` must parse; the package name must be valid.
//! - From target SDK 30, `resources.arsc` must be stored uncompressed and
//!   4-byte aligned, and the APK must be signed with scheme v2 or later.
//! - With `extractNativeLibs="false"`, native libraries must be stored
//!   uncompressed and page (4096) aligned.
//! - `minSdkVersion` above the platform's API level is refused.
//! - A set has exactly one base APK; every split has the base's package
//!   name, version code and signers, and a unique split name.
//!
//! ABI policy (ADR 0028, proposed): Andruna runs 64-bit code only. On x64
//! PCs `x86_64` is preferred and `arm64-v8a` is accepted (translated, phase
//! 5); on ARM64 PCs only `arm64-v8a`. A package whose native code is only
//! for other ABIs (`armeabi-v7a`, `x86`, ...) is refused (P-14).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::arsc::{Preferences, ResTable};
use crate::error::{Code, Error, Result};
use crate::manifest::{self, Manifest, ResString};
use crate::sig::{self, Scheme, SigningDetails, VerifyOptions};
use crate::sys::MappedFile;
use crate::zip::{self, Archive};

/// Largest `AndroidManifest.xml` or `resources.arsc` read (bytes).
const MAX_META: u64 = 64 << 20;

/// One parsed and verified APK.
#[derive(Clone, Debug)]
pub struct ParsedApk {
    pub path: PathBuf,
    pub manifest: Manifest,
    pub signing: SigningDetails,
    /// ABIs with libraries under `lib/<abi>/`, sorted.
    pub abis: Vec<String>,
    pub size: u64,
}

/// The ABIs this PC can run, most preferred first.
pub fn supported_abis() -> Vec<String> {
    if cfg!(target_arch = "aarch64") {
        vec!["arm64-v8a".into()]
    } else {
        vec!["x86_64".into(), "arm64-v8a".into()]
    }
}

fn native_abis(apk: &Archive<'_>) -> Vec<String> {
    let mut set = BTreeSet::new();
    for e in apk.entries() {
        if let Some(rest) = e.name.strip_prefix("lib/")
            && let Some((abi, file)) = rest.split_once('/')
            && !file.contains('/')
            && file.ends_with(".so")
        {
            set.insert(abi.to_string());
        }
    }
    set.into_iter().collect()
}

/// Parses and verifies one APK held in memory.
pub fn parse_bytes(path: &Path, bytes: &[u8]) -> Result<ParsedApk> {
    let ctx = |e: Error| {
        e.context(
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        )
    };
    let apk = Archive::parse(bytes).map_err(ctx)?;
    let entry = apk.find("AndroidManifest.xml").ok_or_else(|| {
        ctx(Error::new(
            Code::InvalidManifest,
            "the APK has no AndroidManifest.xml",
        ))
    })?;
    let m = manifest::parse(&apk.read_limited(entry, MAX_META).map_err(ctx)?).map_err(ctx)?;
    if m.target_sdk >= 30
        && let Some(r) = apk.find("resources.arsc")
        && (r.method != zip::STORED || r.data_offset % 4 != 0)
    {
        return Err(ctx(Error::new(
            Code::Alignment,
            "apps that target API 30 or later must store resources.arsc uncompressed and 4-byte aligned",
        )));
    }
    if !m.extract_native_libs {
        for e in apk.entries() {
            if e.name.starts_with("lib/")
                && e.name.ends_with(".so")
                && (e.method != zip::STORED || e.data_offset % 4096 != 0)
            {
                return Err(ctx(Error::new(
                    Code::Alignment,
                    format!(
                        "`{}` must be stored uncompressed and page aligned (extractNativeLibs is false)",
                        e.name
                    ),
                )));
            }
        }
    }
    let signing = sig::verify(&apk, VerifyOptions::default()).map_err(ctx)?;
    if m.target_sdk >= 30 && signing.scheme == Scheme::V1 {
        return Err(ctx(Error::new(
            Code::InvalidSignature,
            "apps that target API 30 or later must be signed with APK Signature Scheme v2 or later",
        )));
    }
    Ok(ParsedApk {
        path: path.to_path_buf(),
        abis: native_abis(&apk),
        manifest: m,
        signing,
        size: bytes.len() as u64,
    })
}

/// Maps, parses and verifies one APK file.
pub fn parse_file(path: &Path) -> Result<ParsedApk> {
    let map = MappedFile::open(path)
        .map_err(|e| Error::new(Code::Io, format!("{}: {e}", path.display())))?;
    parse_bytes(path, &map)
}

/// A verified set: one base and its splits.
#[derive(Clone, Debug)]
pub struct ApkSet {
    pub base: ParsedApk,
    pub splits: Vec<ParsedApk>,
    /// The ABI whose native libraries are installed; `None` without native
    /// code.
    pub abi: Option<String>,
}

impl ApkSet {
    pub fn package(&self) -> &str {
        &self.base.manifest.package
    }

    pub fn all(&self) -> impl Iterator<Item = &ParsedApk> {
        std::iter::once(&self.base).chain(self.splits.iter())
    }

    /// Native ABIs across the set, sorted.
    pub fn abis(&self) -> Vec<String> {
        let mut v: Vec<String> = self.all().flat_map(|a| a.abis.iter().cloned()).collect();
        v.sort();
        v.dedup();
        v
    }

    pub fn size(&self) -> u64 {
        self.all().map(|a| a.size).sum()
    }
}

/// The ABI a config split is for (`config.arm64_v8a` -> `arm64-v8a`).
fn split_abi(name: &str) -> Option<String> {
    let rest = name.strip_prefix("config.")?;
    let abi = rest.replace('_', "-");
    let known = [
        "arm64-v8a",
        "armeabi-v7a",
        "armeabi",
        "x86",
        "x86-64",
        "mips",
        "mips64",
        "riscv64",
    ];
    known
        .contains(&abi.as_str())
        .then(|| abi.replace("x86-64", "x86_64"))
}

/// The density of a config split (`config.xxhdpi` -> 480).
fn split_density(name: &str) -> Option<u32> {
    Some(match name.strip_prefix("config.")? {
        "ldpi" => 120,
        "mdpi" => 160,
        "tvdpi" => 213,
        "hdpi" => 240,
        "xhdpi" => 320,
        "xxhdpi" => 480,
        "xxxhdpi" => 640,
        _ => return None,
    })
}

/// Target density for split selection: xxhdpi, which suits PC displays at
/// 200% to 300% scaling and downsamples cleanly at 100%.
pub const SPLIT_DENSITY: u32 = 480;

/// Checks that `apks` form one package, drops config splits for other
/// ABIs and densities, and selects the ABI.
pub fn assemble(apks: Vec<ParsedApk>, supported: &[String]) -> Result<ApkSet> {
    let mut bases = apks.iter().filter(|a| a.manifest.split.is_none());
    let base = bases
        .next()
        .ok_or_else(|| Error::new(Code::InvalidSplits, "the set has no base APK"))?
        .clone();
    if bases.next().is_some() {
        return Err(Error::new(
            Code::InvalidSplits,
            "the set has more than one base APK",
        ));
    }
    let mut splits: Vec<ParsedApk> = Vec::new();
    for a in apks.into_iter().filter(|a| a.manifest.split.is_some()) {
        let name = a.manifest.split.clone().unwrap_or_default();
        if a.manifest.package != base.manifest.package {
            return Err(Error::new(
                Code::InvalidSplits,
                format!(
                    "split `{name}` is for package {}, not {}",
                    a.manifest.package, base.manifest.package
                ),
            ));
        }
        if a.manifest.version_code != base.manifest.version_code {
            return Err(Error::new(
                Code::InvalidSplits,
                format!("split `{name}` has another version code"),
            ));
        }
        if a.signing.signers != base.signing.signers {
            return Err(Error::new(
                Code::InvalidSplits,
                format!("split `{name}` is signed by another key"),
            ));
        }
        if splits.iter().any(|s| s.manifest.split == a.manifest.split) {
            return Err(Error::new(
                Code::InvalidSplits,
                format!("split `{name}` is given twice"),
            ));
        }
        splits.push(a);
    }
    if base.manifest.min_sdk > crate::PLATFORM_SDK {
        return Err(Error::new(
            Code::SdkTooNew,
            format!(
                "the app needs Android API level {}; Andruna provides {}",
                base.manifest.min_sdk,
                crate::PLATFORM_SDK
            ),
        ));
    }
    // ABI: from the libraries in the base and in ABI splits.
    let mut available: BTreeSet<String> = base.abis.iter().cloned().collect();
    for s in &splits {
        if let Some(abi) = s.manifest.split.as_deref().and_then(split_abi) {
            available.insert(abi);
        }
        available.extend(s.abis.iter().cloned());
    }
    let abi = if available.is_empty() {
        None
    } else {
        match supported.iter().find(|a| available.contains(*a)) {
            Some(a) => Some(a.clone()),
            None => {
                return Err(Error::new(
                    Code::IncompatibleAbi,
                    format!(
                        "the app's native code is for {}, which this PC cannot run (it runs {})",
                        available.into_iter().collect::<Vec<_>>().join(", "),
                        supported.join(", ")
                    ),
                ));
            }
        }
    };
    // Keep the selected ABI's split and one density split.
    let best_density = splits
        .iter()
        .filter_map(|s| s.manifest.split.as_deref().and_then(split_density))
        .min_by_key(|&d| {
            if d >= SPLIT_DENSITY {
                d - SPLIT_DENSITY
            } else {
                10_000 + SPLIT_DENSITY - d
            }
        });
    splits.retain(|s| {
        let name = s.manifest.split.as_deref().unwrap_or("");
        if let Some(a) = split_abi(name) {
            return Some(a) == abi;
        }
        if let Some(d) = split_density(name) {
            return Some(d) == best_density;
        }
        true
    });
    splits.sort_by(|a, b| a.manifest.split.cmp(&b.manifest.split));
    Ok(ApkSet { base, splits, abi })
}

/// Labels of the application, by locale tag (`""` default, `he`,
/// `fr-CA`), resolved from the base APK's resources.
pub fn labels(table: Option<&ResTable>, label: Option<&ResString>) -> Vec<(String, String)> {
    match label {
        None => Vec::new(),
        Some(ResString::Literal(s)) => vec![(String::new(), s.clone())],
        Some(ResString::Resource(id)) => {
            let Some(t) = table else { return Vec::new() };
            let mut out = Vec::new();
            if let Some(r) = t.resolve(*id, &Preferences::default())
                && let Some(s) = r.value.as_str()
            {
                out.push((String::new(), s.to_string()));
            }
            for (lang, region) in t.locales_of(*id) {
                let tag = if region.is_empty() {
                    lang.clone()
                } else {
                    format!("{lang}-{region}")
                };
                if let Some(r) = t.resolve(*id, &Preferences::for_locale(&tag))
                    && let Some(s) = r.value.as_str()
                {
                    out.push((tag, s.to_string()));
                }
            }
            out
        }
    }
}

/// Picks the label for a locale tag from [`labels`] output: exact tag,
/// then language, then the default.
pub fn label_for(labels: &[(String, String)], locale: &str) -> Option<String> {
    let p = Preferences::for_locale(locale);
    let (lang, region) = p.locales.first().cloned().unwrap_or_default();
    let full = format!("{lang}-{region}");
    labels
        .iter()
        .find(|(t, _)| !region.is_empty() && *t == full)
        .or_else(|| labels.iter().find(|(t, _)| *t == lang))
        .or_else(|| labels.iter().find(|(t, _)| t.is_empty()))
        .or_else(|| labels.first())
        .map(|(_, l)| l.clone())
}
