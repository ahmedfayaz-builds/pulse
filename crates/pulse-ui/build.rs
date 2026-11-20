fn main() {
    slint_build::compile("ui/app.slint").expect("Slint UI compilation failed");

    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/pulse.ico");
        res.compile().expect("Failed to embed Windows icon resource");
    }
}
