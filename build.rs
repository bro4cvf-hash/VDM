fn main() {
    let builder = std::thread::Builder::new().stack_size(8 * 1024 * 1024);
    let handler = builder
        .spawn(|| {
            slint_build::compile("ui/app-window.slint").expect("slint build failed");
        })
        .expect("spawn build thread");
    handler.join().expect("slint build thread failed");

    // brand icon + metadata for the exe (taskbar, pinned shortcuts, explorer)
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/app.ico");
        res.set("FileDescription", "VDM Downloader");
        res.set("ProductName", "VDM");
        res.set("CompanyName", "VDM Contributors");
        res.set("LegalCopyright", "Copyright © 2026 VDM Contributors");
        res.compile().expect("embed windows resources");
    }
}
