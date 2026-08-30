fn main() {
    slint_build::compile("ui/app-window.slint").expect("slint build failed");

    // brand icon + metadata for the exe (taskbar, pinned shortcuts, explorer)
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/app.ico");
        res.set("FileDescription", "VDM Downloader");
        res.set("ProductName", "VDM");
        res.compile().expect("embed windows resources");
    }
}
