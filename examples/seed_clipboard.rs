fn main() {
    let mut c = arboard::Clipboard::new().unwrap();
    for t in [
        "git commit -m \"fix tray icon\"",
        "https://github.com/berchj/clipswitch",
        "Hola! Te paso la lista:\nPan, leche, café\nGracias",
        "pedidos@ejemplo.com",
    ] {
        c.set_text(t).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1800));
    }
}
