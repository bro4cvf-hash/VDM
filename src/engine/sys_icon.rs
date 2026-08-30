//! Extract Windows Shell associated file icons (e.g. WinRAR, 7-Zip, Adobe, etc.)
//! and convert to Slint Images for the Download Info Dialog and Task List.

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

#[cfg(windows)]
mod win {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::null_mut;

    #[repr(C)]
    struct SHFILEINFOW {
        h_icon: *mut std::ffi::c_void,
        i_icon: i32,
        dw_attributes: u32,
        sz_display_name: [u16; 260],
        sz_type_name: [u16; 80],
    }

    #[repr(C)]
    struct ICONINFO {
        f_icon: i32,
        x_hotspot: u32,
        y_hotspot: u32,
        hbm_mask: *mut std::ffi::c_void,
        hbm_color: *mut std::ffi::c_void,
    }

    #[repr(C)]
    struct BITMAPINFOHEADER {
        bi_size: u32,
        bi_width: i32,
        bi_height: i32,
        bi_planes: u16,
        bi_bit_count: u16,
        bi_compression: u32,
        bi_size_image: u32,
        bi_x_pels_per_meter: i32,
        bi_y_pels_per_meter: i32,
        bi_clr_used: u32,
        bi_clr_important: u32,
    }

    #[repr(C)]
    struct BITMAPINFO {
        bmi_header: BITMAPINFOHEADER,
        bmi_colors: [u32; 1],
    }

    const FILE_ATTRIBUTE_NORMAL: u32 = 0x00000080;
    const SHGFI_USEFILEATTRIBUTES: u32 = 0x00000010;
    const SHGFI_ICON: u32 = 0x00000100;
    const SHGFI_LARGEICON: u32 = 0x00000000;
    const DIB_RGB_COLORS: u32 = 0;
    const BI_RGB: u32 = 0;

    #[link(name = "shell32")]
    #[link(name = "user32")]
    #[link(name = "gdi32")]
    extern "system" {
        fn SHGetFileInfoW(
            psz_path: *const u16,
            dw_file_attributes: u32,
            psfi: *mut SHFILEINFOW,
            cb_file_info: u32,
            u_flags: u32,
        ) -> usize;

        fn GetIconInfo(h_icon: *mut std::ffi::c_void, piconinfo: *mut ICONINFO) -> i32;
        fn DestroyIcon(h_icon: *mut std::ffi::c_void) -> i32;
        fn DeleteObject(ho: *mut std::ffi::c_void) -> i32;
        fn GetDC(h_wnd: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
        fn ReleaseDC(h_wnd: *mut std::ffi::c_void, h_dc: *mut std::ffi::c_void) -> i32;
        fn GetDIBits(
            hdc: *mut std::ffi::c_void,
            hbm: *mut std::ffi::c_void,
            start: u32,
            c_lines: u32,
            lpv_bits: *mut std::ffi::c_void,
            lpbmi: *mut BITMAPINFO,
            usage: u32,
        ) -> i32;
    }

    pub fn get_icon_rgba(filename: &str) -> Option<(Vec<u8>, u32, u32)> {
        let ext = if let Some(dot_idx) = filename.rfind('.') {
            &filename[dot_idx..]
        } else {
            filename
        };
        let dummy_name = if ext.starts_with('.') {
            format!("dummy{}", ext)
        } else {
            format!("dummy.{}", ext)
        };

        let wide: Vec<u16> = OsStr::new(&dummy_name).encode_wide().chain(Some(0)).collect();
        let mut shfi: SHFILEINFOW = unsafe { std::mem::zeroed() };

        let res = unsafe {
            SHGetFileInfoW(
                wide.as_ptr(),
                FILE_ATTRIBUTE_NORMAL,
                &mut shfi,
                std::mem::size_of::<SHFILEINFOW>() as u32,
                SHGFI_USEFILEATTRIBUTES | SHGFI_ICON | SHGFI_LARGEICON,
            )
        };

        if res == 0 || shfi.h_icon.is_null() {
            return None;
        }

        let h_icon = shfi.h_icon;
        let mut icon_info: ICONINFO = unsafe { std::mem::zeroed() };
        if unsafe { GetIconInfo(h_icon, &mut icon_info) } == 0 {
            unsafe { DestroyIcon(h_icon) };
            return None;
        }

        let hdc = unsafe { GetDC(null_mut()) };
        let width = 32u32;
        let height = 32u32;
        let mut bi: BITMAPINFO = unsafe { std::mem::zeroed() };
        bi.bmi_header.bi_size = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bi.bmi_header.bi_width = width as i32;
        bi.bmi_header.bi_height = -(height as i32); // Top-down DIB
        bi.bmi_header.bi_planes = 1;
        bi.bmi_header.bi_bit_count = 32;
        bi.bmi_header.bi_compression = BI_RGB;

        let mut bgra_buf = vec![0u8; (width * height * 4) as usize];
        let target_bitmap = if !icon_info.hbm_color.is_null() {
            icon_info.hbm_color
        } else {
            icon_info.hbm_mask
        };

        let lines = unsafe {
            GetDIBits(
                hdc,
                target_bitmap,
                0,
                height,
                bgra_buf.as_mut_ptr() as *mut std::ffi::c_void,
                &mut bi,
                DIB_RGB_COLORS,
            )
        };

        unsafe {
            if !icon_info.hbm_color.is_null() {
                DeleteObject(icon_info.hbm_color);
            }
            if !icon_info.hbm_mask.is_null() {
                DeleteObject(icon_info.hbm_mask);
            }
            ReleaseDC(null_mut(), hdc);
            DestroyIcon(h_icon);
        }

        if lines == 0 {
            return None;
        }

        // Convert BGRA to RGBA and check alpha
        let mut rgba = vec![0u8; bgra_buf.len()];
        let mut has_alpha = false;
        for i in 0..(width * height) as usize {
            let b = bgra_buf[i * 4];
            let g = bgra_buf[i * 4 + 1];
            let r = bgra_buf[i * 4 + 2];
            let a = bgra_buf[i * 4 + 3];
            if a > 0 {
                has_alpha = true;
            }
            rgba[i * 4] = r;
            rgba[i * 4 + 1] = g;
            rgba[i * 4 + 2] = b;
            rgba[i * 4 + 3] = a;
        }

        // If icon had no alpha channel (e.g. 24-bit icon), set alpha to 255 for non-transparent pixels
        if !has_alpha {
            for i in 0..(width * height) as usize {
                let r = rgba[i * 4];
                let g = rgba[i * 4 + 1];
                let b = rgba[i * 4 + 2];
                rgba[i * 4 + 3] = if r == 0 && g == 0 && b == 0 { 0 } else { 255 };
            }
        }

        Some((rgba, width, height))
    }
}

pub fn get_file_icon_image(filename: &str) -> Option<Image> {
    #[cfg(windows)]
    {
        if let Some((rgba, w, h)) = win::get_icon_rgba(filename) {
            let pixel_buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&rgba, w, h);
            return Some(Image::from_rgba8(pixel_buf));
        }
    }
    #[allow(unreachable_code)]
    None
}
