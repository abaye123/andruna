//! Unicode character properties for bionic's `wctype.cpp` and
//! `wcwidth.cpp` (`private/icu4x.h`).
//!
//! On Android these functions are a thin Rust wrapper around ICU4X inside
//! `libc.so`. Here they are host functions answered by the ICU that
//! Windows ships (`icuuc.dll`), with the same properties: general
//! category, East Asian width, Hangul syllable type, the binary properties
//! and the POSIX-compatible classes, simple case mappings. The answers
//! follow Windows' Unicode version, which can differ from Android's for
//! characters added in between.

use windows::Win32::Globalization::{
    UCHAR_ALPHABETIC, UCHAR_DEFAULT_IGNORABLE_CODE_POINT, UCHAR_EAST_ASIAN_WIDTH,
    UCHAR_HANGUL_SYLLABLE_TYPE, UCHAR_LOWERCASE, UCHAR_POSIX_ALNUM, UCHAR_POSIX_BLANK,
    UCHAR_POSIX_GRAPH, UCHAR_POSIX_PRINT, UCHAR_POSIX_XDIGIT, UCHAR_UPPERCASE, UCHAR_WHITE_SPACE,
    UProperty, u_charType, u_getIntPropertyValue, u_hasBinaryProperty, u_tolower, u_toupper,
};

fn cp(c: u32) -> Option<i32> {
    (c <= 0x10_ffff).then_some(c as i32)
}

fn has(c: u32, p: UProperty) -> bool {
    // SAFETY: ICU accepts any code point value.
    cp(c).is_some_and(|c| unsafe { u_hasBinaryProperty(c, p) } != 0)
}

fn int_prop(c: u32, p: UProperty) -> u8 {
    // SAFETY: as above.
    cp(c).map_or(0, |c| unsafe { u_getIntPropertyValue(c, p) } as u8)
}

macro_rules! binary {
    ($($name:ident => $prop:ident),* $(,)?) => {
        $(gfn! { unsafe fn $name(c: u32) -> bool { has(c, $prop) } })*
    };
}

binary! {
    is_alphabetic => UCHAR_ALPHABETIC,
    is_default_ignorable => UCHAR_DEFAULT_IGNORABLE_CODE_POINT,
    is_lowercase => UCHAR_LOWERCASE,
    is_alnum => UCHAR_POSIX_ALNUM,
    is_blank => UCHAR_POSIX_BLANK,
    is_graph => UCHAR_POSIX_GRAPH,
    is_print => UCHAR_POSIX_PRINT,
    is_xdigit => UCHAR_POSIX_XDIGIT,
    is_white_space => UCHAR_WHITE_SPACE,
    is_uppercase => UCHAR_UPPERCASE,
}

gfn! {
    unsafe fn general_category(c: u32) -> u8 {
        // SAFETY: as above.
        cp(c).map_or(0, |c| unsafe { u_charType(c) } as u8)
    }
}

gfn! {
    unsafe fn east_asian_width(c: u32) -> u8 {
        int_prop(c, UCHAR_EAST_ASIAN_WIDTH)
    }
}

gfn! {
    unsafe fn hangul_syllable_type(c: u32) -> u8 {
        int_prop(c, UCHAR_HANGUL_SYLLABLE_TYPE)
    }
}

gfn! {
    unsafe fn to_upper(c: u32) -> u32 {
        // SAFETY: as above.
        cp(c).map_or(c, |v| unsafe { u_toupper(v) } as u32)
    }
}

gfn! {
    unsafe fn to_lower(c: u32) -> u32 {
        // SAFETY: as above.
        cp(c).map_or(c, |v| unsafe { u_tolower(v) } as u32)
    }
}

pub fn symbols() -> Vec<(&'static str, u64)> {
    vec![
        ("__icu4x_bionic_general_category", general_category as usize as u64),
        ("__icu4x_bionic_east_asian_width", east_asian_width as usize as u64),
        ("__icu4x_bionic_hangul_syllable_type", hangul_syllable_type as usize as u64),
        ("__icu4x_bionic_is_alphabetic", is_alphabetic as usize as u64),
        ("__icu4x_bionic_is_default_ignorable_code_point", is_default_ignorable as usize as u64),
        ("__icu4x_bionic_is_lowercase", is_lowercase as usize as u64),
        ("__icu4x_bionic_is_alnum", is_alnum as usize as u64),
        ("__icu4x_bionic_is_blank", is_blank as usize as u64),
        ("__icu4x_bionic_is_graph", is_graph as usize as u64),
        ("__icu4x_bionic_is_print", is_print as usize as u64),
        ("__icu4x_bionic_is_xdigit", is_xdigit as usize as u64),
        ("__icu4x_bionic_is_white_space", is_white_space as usize as u64),
        ("__icu4x_bionic_is_uppercase", is_uppercase as usize as u64),
        ("__icu4x_bionic_to_upper", to_upper as usize as u64),
        ("__icu4x_bionic_to_lower", to_lower as usize as u64),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn properties() {
        // SAFETY: host functions called directly.
        unsafe {
            assert!(is_alphabetic('a' as u32) && is_alphabetic(0x05d0));
            assert!(!is_alphabetic('1' as u32));
            assert!(is_uppercase('A' as u32) && is_lowercase(0x00e9));
            assert!(is_white_space(0x2003) && is_blank('\t' as u32));
            assert!(is_xdigit('f' as u32) && !is_xdigit('g' as u32));
            assert_eq!(to_upper(0x00e9), 0x00c9);
            assert_eq!(to_lower('Q' as u32), 'q' as u32);
            assert_eq!(general_category(0x0301), 6, "non-spacing mark");
            assert_eq!(general_category('5' as u32), 9, "decimal number");
            assert_eq!(east_asian_width(0x4e00), 5, "wide");
            assert_eq!(east_asian_width('a' as u32), 4, "narrow");
            assert_eq!(hangul_syllable_type(0x1100), 1, "leading jamo");
            assert_eq!(hangul_syllable_type(0xac00), 4, "LV syllable");
            assert!(is_default_ignorable(0x200b));
            assert_eq!(to_upper(0x11_0000), 0x11_0000);
        }
    }
}
