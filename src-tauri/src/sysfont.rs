//! 枚举系统已装的字体，给 GUI 的字体菜单用。
//!
//! 走 GDI 的 `EnumFontFamiliesExW`，而不是去读注册表：注册表那条路会把
//! 「Arial Bold」「Arial Italic」这类**字形族**当成独立条目报出来，菜单里全是
//! 重复项；GDI 按字体族枚举，一个族只回一条。
//!
//! 只做枚举，不做缓存 —— 字体是用户装完就基本不变的，界面每次打开问一次就够。

/// 系统已装字体族名，按名字排序、去重。
///
/// 拿不到（非 Windows、或 GDI 出错）就返回空表：界面上会退回到「手填字体名」，
/// 不该为这个把界面卡住。
#[cfg(windows)]
pub fn list() -> Vec<String> {
    use windows_sys::Win32::Foundation::LPARAM;
    use windows_sys::Win32::Graphics::Gdi::{
        EnumFontFamiliesExW, GetDC, ReleaseDC, DEFAULT_CHARSET, LOGFONTW, TEXTMETRICW,
    };

    /// 回调的累计器，用 `lParam` 传进去。
    struct Acc {
        names: std::collections::BTreeSet<String>,
    }

    /// GDI 每枚举到一个族就回调一次；返回非 0 = 继续枚举。
    unsafe extern "system" fn collect(
        lf: *const LOGFONTW,
        _tm: *const TEXTMETRICW,
        _font_type: u32,
        lparam: LPARAM,
    ) -> i32 {
        if !lf.is_null() {
            let acc = &mut *(lparam as *mut Acc);
            let raw = &(*lf).lfFaceName;
            let len = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
            let name = String::from_utf16_lossy(&raw[..len]);
            // `@` 开头的是竖排变体（`@宋体` 这种），和横排那条重名，丢掉。
            if !name.is_empty() && !name.starts_with('@') {
                acc.names.insert(name);
            }
        }
        1
    }

    let mut acc = Acc {
        names: std::collections::BTreeSet::new(),
    };

    unsafe {
        // 传空 HWND = 取整个屏幕的 DC；枚举字体只要有个 DC 就行。
        let hdc = GetDC(std::ptr::null_mut());
        if hdc.is_null() {
            return Vec::new();
        }
        // 空 LOGFONT + DEFAULT_CHARSET = 「这个 DC 上所有字符集的字体族都要」。
        // 同一个族会按字符集回来好几次（GB2312 一次、Shift-JIS 一次…），
        // 上面的 BTreeSet 顺手去重。
        let mut lf: LOGFONTW = std::mem::zeroed();
        lf.lfCharSet = DEFAULT_CHARSET;
        EnumFontFamiliesExW(hdc, &lf, Some(collect), &mut acc as *mut Acc as LPARAM, 0);
        ReleaseDC(std::ptr::null_mut(), hdc);
    }

    acc.names.into_iter().collect()
}

/// 非 Windows 上没有这套 GDI 接口，给个空表让界面退回手填。
#[cfg(not(windows))]
pub fn list() -> Vec<String> {
    Vec::new()
}